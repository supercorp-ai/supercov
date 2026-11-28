// How much longer a public project's tests take under Supercov than without
// it, for one non-JavaScript project (SPEED_PROJECT names one of `projects`).
// Plain and each binary of SUPERCOV_BINARIES (`name=path,...`) take turns,
// in an order that turns each round (ABBA), after one cold run of each that
// fills the caches. The project's `.supercov` is kept between runs, as an
// agent's loop keeps it, so a compiled language reuses its instrumented build
// as the plain run reuses its own. Each Supercov run writes its answers
// (answers.mjs) so binaries can be compared byte for byte.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const projects = {
  'rust-semver': {
    repo: 'https://github.com/dtolnay/semver', ref: '1.0.23',
    test: 'cargo test',
  },
  'rust-ripgrep': {
    repo: 'https://github.com/BurntSushi/ripgrep', ref: '14.1.1',
    test: 'cargo test --workspace',
    rounds: 1,
  },
  'rust-bytes': {
    repo: 'https://github.com/tokio-rs/bytes', ref: 'v1.7.2',
    test: 'cargo test',
  },
  'rust-serde-json': {
    repo: 'https://github.com/serde-rs/json', ref: 'v1.0.128',
    test: 'cargo test',
  },
  'python-click': {
    repo: 'https://github.com/pallets/click', ref: '8.1.7',
    setup: 'python -m venv .venv && .venv/bin/pip install -q -e . pytest==8.3.3',
    test: '.venv/bin/python -m pytest -q -p no:cacheprovider',
  },
  'python-h11': {
    repo: 'https://github.com/python-hyper/h11', ref: 'v0.14.0',
    setup: 'python -m venv .venv && .venv/bin/pip install -q -e . pytest',
    test: '.venv/bin/python -m pytest -q -p no:cacheprovider',
  },
  'go-cobra': {
    repo: 'https://github.com/spf13/cobra', ref: 'v1.8.1',
    setup: 'go mod download',
    test: 'go test -count=1 ./...',
  },
  'go-gin': {
    repo: 'https://github.com/gin-gonic/gin', ref: 'v1.10.0',
    setup: 'go mod download',
    test: 'go test -count=1 ./...',
  },
  'ruby-rack': {
    repo: 'https://github.com/rack/rack', ref: 'v3.1.8',
    setup: 'bundle config set --local path vendor/bundle && bundle install --quiet',
    test: 'bundle exec rake test TESTOPTS=--seed=1',
  },
  'python-jinja': {
    repo: 'https://github.com/pallets/jinja', ref: '3.1.4',
    setup: 'python -m venv .venv && .venv/bin/pip install -q -e . pytest==8.3.3 trio',
    test: '.venv/bin/python -m pytest -q -p no:cacheprovider',
  },
  'ruby-liquid': {
    repo: 'https://github.com/Shopify/liquid', ref: 'v5.5.0',
    setup: 'bundle config set --local path vendor/bundle && bundle install --quiet',
    test: 'bundle exec rake base_test TESTOPTS=--seed=1',
  },
  'jvm-gson': {
    repo: 'https://github.com/google/gson', ref: 'gson-parent-2.11.0',
    setup: 'mvn -q -B -DskipTests install -pl gson -am && mvn -q -B test -pl gson',
    test: 'mvn -q -B test -pl gson',
  },
  'jvm-commons-cli': {
    repo: 'https://github.com/apache/commons-cli', ref: 'rel/commons-cli-1.9.0',
    setup: 'mvn -q -B -Drat.skip -Djapicmp.skip install',
    test: 'mvn -q -B -Drat.skip -Djapicmp.skip -Danimal.sniffer.skip -Dcheckstyle.skip -Dspotbugs.skip -Dpmd.skip test',
  },
};

const name = process.env.SPEED_PROJECT;
const project = projects[name];
assert.ok(project, `SPEED_PROJECT names no project: ${name}`);
const binaries = (process.env.SUPERCOV_BINARIES ?? '').split(',').filter(Boolean).map((entry) => {
  const [label, path] = entry.split('=');
  return { label, path: resolve(path) };
});
const rounds = projects[process.env.SPEED_PROJECT]?.rounds ?? Number(process.env.SPEED_ROUNDS ?? 2);
const output = resolve(process.env.SPEED_OUTPUT ?? 'speed-languages');
mkdirSync(join(output, 'logs'), { recursive: true });
mkdirSync(join(output, 'answers'), { recursive: true });
const answers = resolve(import.meta.dirname, 'answers.mjs');

function shell(cwd, command, label, mayFail = false) {
  const started = process.hrtime.bigint();
  const result = spawnSync('bash', ['-c', command], {
    cwd, encoding: 'utf8', timeout: 3_600_000, maxBuffer: 1 << 30,
    env: { ...process.env, CI: 'true', SUPERCOV_PHASE_TIMING: '1' },
  });
  const seconds = Number(process.hrtime.bigint() - started) / 1e9;
  writeFileSync(join(output, 'logs', `${label}.log`), `$ ${command}\nexit ${result.status}\n${result.stdout}${result.stderr}`);
  if (!mayFail) assert.equal(result.status, 0, `${label} failed (exit ${result.status}): see logs/${label}.log\n${(result.stdout + result.stderr).slice(-3000)}`);
  return { seconds, status: result.status, text: result.stdout + result.stderr };
}

// Each binary measures its own clone, so one's cache never serves another.
function clone(label) {
  const dir = resolve(output, '..', `project-${label}`);
  if (!existsSync(dir)) {
    shell(resolve(output, '..'), `git clone -q --depth 1 --branch ${project.ref} ${project.repo} ${dir}`, `clone-${label}`);
    if (project.setup) shell(dir, project.setup, `setup-${label}`);
  }
  return dir;
}

function newestRun(dir) {
  const runs = join(dir, '.supercov', 'runs');
  if (!existsSync(runs)) return undefined;
  const entries = readdirSync(runs).filter((entry) => statSync(join(runs, entry)).isDirectory());
  entries.sort((a, b) => statSync(join(runs, b)).mtimeMs - statSync(join(runs, a)).mtimeMs);
  return entries[0];
}

function findTimings(value) {
  if (!value || typeof value !== 'object') return undefined;
  if (value.timings && typeof value.timings === 'object' && 'testCommandMs' in value.timings) return value.timings;
  for (const child of Object.values(value)) {
    const found = findTimings(child);
    if (found) return found;
  }
  return undefined;
}

function runTimings(dir, run) {
  const runDir = join(dir, '.supercov', 'runs', run);
  for (const file of readdirSync(runDir).filter((entry) => entry.endsWith('.json'))) {
    try {
      const found = findTimings(JSON.parse(readFileSync(join(runDir, file), 'utf8')));
      if (found) return found;
    } catch {}
  }
  return undefined;
}

const samples = join(output, 'samples.jsonl');
function record(sample) {
  appendFileSync(samples, `${JSON.stringify(sample)}\n`);
  console.log(JSON.stringify(sample));
}

const plainDir = clone('plain');
const dirs = Object.fromEntries(binaries.map(({ label }) => [label, clone(label)]));

function measure(variant, round, kind) {
  if (variant === 'plain') {
    const { seconds, status } = shell(plainDir, project.test, `${kind}-${round}-plain`, true);
    record({ project: name, variant, round, kind, seconds, status });
    return;
  }
  const binary = binaries.find(({ label }) => label === variant);
  const dir = dirs[variant];
  const { seconds, status } = shell(dir, `${binary.path} -- ${project.test}`, `${kind}-${round}-${variant}`, true);
  const run = newestRun(dir);
  const timings = run ? runTimings(dir, run) : undefined;
  record({ project: name, variant, round, kind, seconds, status, run, timings });
  if (run && kind === 'warm' && round <= (process.env.SPEED_FULL_ANSWERS === '1' ? 0 : 1)) {
    // Two of each binary: what differs between them is the suite's own
    // variation, against which the binaries are compared.
    const out = join(output, 'answers', round === 0 ? `${variant}.json` : `${variant}-2.json`);
    // Every file, line and decision where the evidence's records change
    // shape between binaries (SPEED_FULL_ANSWERS=1): those answers are
    // compared, the records are not.
    const quick = process.env.SPEED_FULL_ANSWERS === '1' ? [] : ['--quick'];
    const result = spawnSync('node', [answers, binary.path, dir, run, out, ...quick], { encoding: 'utf8', maxBuffer: 1 << 30 });
    if (result.status !== 0) console.log(`answers for ${variant} failed: ${result.stderr.slice(-2000)}`);
  }
}

const variants = ['plain', ...binaries.map(({ label }) => label)];
// The cold runs fill each clone's caches (Cargo's target, Go's build cache,
// Supercov's instrumented build) and are reported apart.
for (const variant of variants) measure(variant, 0, 'cold');
for (let round = 0; round < rounds; round += 1) {
  const order = round % 2 === 0 ? variants : [...variants].reverse();
  for (const variant of order) measure(variant, round, 'warm');
}

// Diagnostics, apart from the timed rounds: where the measured run's time goes.
const diagnose = process.env.SPEED_DIAGNOSE === '1' ? binaries[binaries.length - 1] : undefined;
if (diagnose) {
  const dir = dirs[diagnose.label];
  const diag = join(output, 'diag');
  mkdirSync(diag, { recursive: true });
  const workspace = join(dir, '.supercov', 'workspaces', 'workspace', `project-${diagnose.label}`);
  if (name.startsWith('go-')) {
    // The instrumented copy run by plain go: build reuse and probe cost apart
    // from Supercov's own phases.
    for (const attempt of [1, 2]) {
      const { seconds } = shell(workspace, project.test, `diag-go-workspace-${attempt}`, true);
      record({ project: name, variant: 'diag-go-workspace', round: attempt, kind: 'diag', seconds });
    }
    const { seconds } = shell(dir, `${diagnose.path} -- ${project.test.replace('go test', 'go test -x')}`, 'diag-go-x', true);
    record({ project: name, variant: 'diag-go-x', round: 0, kind: 'diag', seconds });
  }
  if (name.startsWith('ruby-')) {
    process.env.SUPERCOV_STACKPROF = diag;
    const { seconds } = shell(dir, `${diagnose.path} -- ${project.test}`, 'diag-ruby-stackprof', true);
    delete process.env.SUPERCOV_STACKPROF;
    record({ project: name, variant: 'diag-stackprof', round: 0, kind: 'diag', seconds });
    shell(dir, `stackprof --text --limit 60 ${diag}/*.dump > ${diag}/stackprof.txt 2>&1; stackprof --text --limit 60 --method 'Supercov::Runtime#probe_decision' ${diag}/*.dump > ${diag}/stackprof-decision.txt 2>&1 || true`, 'diag-ruby-report', true);
  }
  if (process.env.SPEED_PERF && (name.startsWith('python-') || name.startsWith('ruby-') || name.startsWith('go-') || name.startsWith('rust-'))) {
    // Supercov's own process: where its evidence and publication time goes.
    const data = join(diag, 'perf.data');
    process.env.SUPERCOV_VERBOSE = '1';
    const { seconds } = shell(dir, `${process.env.SPEED_PERF} record -F 999 -g -o ${data} -- ${diagnose.path} -- ${project.test}`, 'diag-perf', true);
    delete process.env.SUPERCOV_VERBOSE;
    record({ project: name, variant: 'diag-perf', round: 0, kind: 'diag', seconds });
    shell(dir, `${process.env.SPEED_PERF} report -i ${data} --stdio --no-children --comm supercov --sort symbol --percent-limit 0.3 -g none > ${diag}/perf-supercov.txt 2>&1; ${process.env.SPEED_PERF} report -i ${data} --stdio --children --comm supercov --sort symbol --percent-limit 2 -g none > ${diag}/perf-supercov-inclusive.txt 2>&1; ${process.env.SPEED_PERF} report -i ${data} --stdio --no-children --sort comm,dso --percent-limit 0.5 > ${diag}/perf-processes.txt 2>&1; rm -f ${data}`, 'diag-perf-report', true);
  }
  if (name.startsWith('python-') && process.env.SUPERCOV_PYSPY_BIN) {
    process.env.SUPERCOV_PYSPY = diag;
    const { seconds } = shell(dir, `${diagnose.path} -- ${project.test}`, 'diag-python-pyspy', true);
    delete process.env.SUPERCOV_PYSPY;
    record({ project: name, variant: 'diag-pyspy', round: 0, kind: 'diag', seconds });
    shell(plainDir, `sudo -n ${process.env.SUPERCOV_PYSPY_BIN} record --rate 500 --format raw -o ${diag}/plain.txt -- ${project.test}`, 'diag-python-pyspy-plain', true);
  }
  if (name.startsWith('rust-')) {
    const { seconds } = shell(dir, `${diagnose.path} -- ${project.test} 2>&1 | ts -s '%.s' || true`, 'diag-rust-ts', true);
    record({ project: name, variant: 'diag-rust-ts', round: 0, kind: 'diag', seconds });
  }
}

// Summary: median warm seconds per variant, ratio against plain, and the
// median of each Supercov phase.
const all = readFileSync(samples, 'utf8').trim().split('\n').map((line) => JSON.parse(line)).filter((sample) => sample.project === name);
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const warm = (variant) => all.filter((sample) => sample.kind === 'warm' && sample.variant === variant);
const cold = (variant) => all.find((sample) => sample.kind === 'cold' && sample.variant === variant);
const plain = median(warm('plain').map((sample) => sample.seconds));
const lines = [
  `### ${name} (${project.repo} ${project.ref}: \`${project.test}\`)`,
  '',
  '| Variant | Cold s | Warm s (median) | Ratio | Init | Workspace | Adapter | Build | Test command | Publication |',
  '| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |',
];
for (const variant of variants) {
  const runs = warm(variant);
  const seconds = median(runs.map((sample) => sample.seconds));
  const phase = (key) => {
    const values = runs.map((sample) => sample.timings?.[key]).filter((value) => typeof value === 'number');
    return values.length ? (median(values) / 1000).toFixed(2) : '';
  };
  lines.push(`| ${variant} | ${cold(variant)?.seconds.toFixed(2)} | ${seconds.toFixed(2)} | ${(seconds / plain).toFixed(3)} | ${phase('initializationMs')} | ${phase('workspacePreparationMs')} | ${phase('adapterSetupMs')} | ${phase('instrumentedBuildMs')} | ${phase('testCommandMs')} | ${phase('evidencePublicationMs')} |`);
}
const statuses = all.map((sample) => `${sample.variant}:${sample.status}`);
lines.push('', `Exit statuses: ${[...new Set(statuses)].join(', ')}`, '');
writeFileSync(join(output, 'summary.md'), `${lines.join('\n')}\n`);
console.log(lines.join('\n'));
if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${lines.join('\n')}\n`);
