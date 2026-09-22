#!/usr/bin/env node
// The probe frontend against the monitoring frontend, on the same fixture,
// as a differential gate.
//
// Both frontends measure the conformance fixture; every number the report
// derives must agree: line, branch and condition totals, every decision's
// vectors, which assertion sites a passing test reached, and which test a
// thread's and a subprocess's lines belong to. Then the things only the probe
// frontend has to get right: bytecode it leaves in pytest's cache must not
// break a plain run, and an interpreter older than 3.12 must be measured.
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readdirSync, rmSync, cpSync, statSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const fixture = resolve(repository, 'tests/fixtures/python-monitoring');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-python-probes-'));
const binary = resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  return result;
}

function ok(result, what) {
  assert.equal(result.status, 0, `${what}\n${result.stdout}\n${result.stderr}`);
  return result;
}

function findInterpreter(preferred) {
  const candidates = preferred ? [preferred] : [process.env.SUPERCOV_PYTHON, 'python3.14', 'python3.13', 'python3.12', 'python3'].filter(Boolean);
  for (const candidate of candidates) {
    const probe = run(candidate, ['-c', 'import sys; print(sys.version_info >= (3, 12))']);
    if (probe.status === 0 && probe.stdout.trim() === 'True') return candidate;
  }
  return null;
}

function project(name) {
  const directory = resolve(temporary, name);
  cpSync(fixture, directory, { recursive: true });
  for (const entry of readdirSync(directory, { recursive: true, withFileTypes: true })) {
    if (entry.isDirectory() && entry.name === '__pycache__') rmSync(join(entry.parentPath ?? entry.path, entry.name), { recursive: true, force: true });
  }
  ok(run('git', ['init', '-q', '.'], { cwd: directory }), 'git init');
  return directory;
}

function venv(python, name, packages) {
  const directory = resolve(temporary, name);
  ok(run(python, ['-m', 'venv', directory]), 'venv');
  const interpreter = resolve(directory, process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python');
  ok(run(interpreter, ['-m', 'pip', 'install', '--disable-pip-version-check', '-q', ...packages]), 'pip install');
  return interpreter;
}

function supercov(cwd, frontend, args, extra = {}) {
  const environment = { ...process.env, SUPERCOV_PYTHON_FRONTEND: frontend, ...extra };
  delete environment.SUPERCOV_KEEP_WORK;
  return run(binary, args, { cwd, env: environment });
}

function latestRun(cwd) {
  const runs = resolve(cwd, '.supercov/runs');
  return readdirSync(runs).map((id) => ({ id, at: statSync(join(runs, id)).mtimeMs })).sort((a, b) => b.at - a.at)[0].id;
}

function query(cwd, runId, args) {
  const result = ok(run(binary, ['runs', runId, ...args, '--json'], { cwd }), `query ${args.join(' ')}`);
  const payload = JSON.parse(result.stdout);
  assert.equal(payload.ok, true, result.stdout);
  return payload.data;
}

function summary(cwd, runId) {
  const data = query(cwd, runId, []);
  const c = data.coverage;
  return {
    lines: [c.lines.covered, c.lines.total],
    branches: [c.branches.covered, c.branches.total],
    conditions: [c.coveredConditions, c.conditions],
    tests: data.tests,
    complete: data.measurement.complete,
    variant: data.model.variant,
  };
}

function decisionLocations(cwd) {
  // Every decision the plan names, from the source the fixture measures.
  const source = readFileSync(resolve(cwd, 'app/shapes.py'), 'utf8').split('\n');
  const lines = [];
  source.forEach((text, index) => {
    if (/^\s*(if|elif|while|assert)\b|\bif\b.* else |for .* in .* if /.test(text)) lines.push(index + 1);
  });
  return lines.map((line) => `app/shapes.py:${line}`);
}

function vectors(cwd, runId, location) {
  const decisions = query(cwd, runId, ['decision', location]).decisions ?? [];
  return decisions
    .flatMap((decision) => decision.vectors.map((vector) => `${vector.values.map((v) => (v === null ? '-' : v ? 'T' : 'F')).join('')}->${vector.outcome ? 'T' : 'F'}`))
    .sort();
}

function observedSites(cwd, runId) {
  const sites = [];
  let offset = 0;
  for (;;) {
    const page = query(cwd, runId, ['assertions', '--limit', '200', '--offset', String(offset)]);
    sites.push(...page.items.map((item) => `${item.at.file}:${item.at.line}=${(item.observedPassingTests ?? []).length > 0}`));
    if (page.pagination.nextOffset === null) return sites.sort();
    offset = page.pagination.nextOffset;
  }
}

function lineTests(cwd, runId, location) {
  return (query(cwd, runId, ['line', location]).tests ?? []).map((test) => test.id).sort();
}

try {
  assert.ok(existsSync(binary), `build first: ${binary}`);
  const python = findInterpreter();
  assert.ok(python, 'no CPython 3.12+ on PATH (or SUPERCOV_PYTHON)');
  const interpreter = venv(python, 'venv', ['pytest', 'pytest-xdist', 'pytest-rerunfailures']);
  const cwd = project('fixture');
  const environment = { PATH: `${resolve(interpreter, '..')}${delimiter}${process.env.PATH}` };

  // -- the two frontends on the same suite ---------------------------------
  const runs = {};
  for (const frontend of ['monitoring', 'probes']) {
    const result = supercov(cwd, frontend, ['--', interpreter, '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests'], environment);
    ok(result, `${frontend} run`);
    assert.match(result.stdout + result.stderr, /12 passed, 1 skipped, 1 xfailed/, `${frontend}: the suite's own outcome is unchanged`);
    runs[frontend] = latestRun(cwd);
  }
  const monitoring = summary(cwd, runs.monitoring);
  const probes = summary(cwd, runs.probes);
  for (const key of ['lines', 'branches', 'conditions', 'tests', 'complete']) {
    assert.deepEqual(probes[key], monitoring[key], `${key}: probes ${JSON.stringify(probes[key])} vs monitoring ${JSON.stringify(monitoring[key])}`);
  }
  // The fixture's known totals, so both agreeing on something wrong fails too.
  assert.deepEqual(probes.lines, [81, 82]);
  assert.deepEqual(probes.branches, [79, 94]);
  assert.deepEqual(probes.conditions, [8, 16]);

  const locations = decisionLocations(cwd);
  assert.ok(locations.length >= 10, `found ${locations.length} decision lines`);
  for (const location of locations) {
    assert.deepEqual(vectors(cwd, runs.probes, location), vectors(cwd, runs.monitoring, location), `decision vectors at ${location}`);
  }
  assert.deepEqual(vectors(cwd, runs.probes, 'app/shapes.py:26'), ['TF->T', 'TT->F'], 'not (a and b) keeps both operands as conditions');

  assert.deepEqual(observedSites(cwd, runs.probes), observedSites(cwd, runs.monitoring), 'assertion sites a passing test reached');
  assert.ok(observedSites(cwd, runs.probes).some((site) => site.endsWith('=true')), 'some assertion site was observed');

  for (const location of ['app/shapes.py:66', 'app/shapes.py:34', 'app/shapes.py:85']) {
    assert.deepEqual(lineTests(cwd, runs.probes, location), lineTests(cwd, runs.monitoring, location), `tests attributed to ${location}`);
  }
  assert.ok(lineTests(cwd, runs.probes, 'app/shapes.py:66').some((id) => id.includes('test_thread_and_subprocess')), 'a thread\'s lines belong to the test that started it');

  // -- xdist and reruns under probes -----------------------------------------
  ok(supercov(cwd, 'probes', ['--', interpreter, '-m', 'pytest', '-q', '-p', 'no:cacheprovider', '-n', '2', 'tests'], environment), 'probes under xdist');
  assert.deepEqual(summary(cwd, latestRun(cwd)).lines, [81, 82], 'xdist workers each probe and their evidence joins');
  const rerun = ok(supercov(cwd, 'probes', ['--', interpreter, '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests_extended/test_rerun.py'], environment), 'probes rerun');
  assert.match(rerun.stderr, /1 test\(s\) across 2 source file\(s\)/);
  assert.deepEqual(vectors(cwd, latestRun(cwd), 'app/shapes.py:26'), ['TF->T', 'TT->F']);

  // -- what the probe frontend alone must get right --------------------------
  // pytest keeps the bytecode it compiled; a plain run after a probed run
  // must not import Supercov's runtime from it.
  const plain = run(interpreter, ['-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests'], { cwd, env: { ...process.env, ...environment } });
  ok(plain, 'a plain run after a probed run');
  assert.match(plain.stdout, /12 passed, 1 skipped, 1 xfailed/);
  assert.doesNotMatch(plain.stdout + plain.stderr, /supercov/i, 'nothing of Supercov surfaces in a plain run');

  // Nothing written into the project but pytest's own cache and .supercov.
  const stray = readdirSync(cwd, { recursive: true }).filter((entry) => /\.supercov_|_scv_|supercov_probes/.test(String(entry)) && !String(entry).startsWith('.supercov'));
  assert.deepEqual(stray, [], 'the project holds no Supercov artefacts outside .supercov');

  // -- an interpreter the monitoring frontend cannot measure ------------------
  const older = ['python3.11', 'python3.10', 'python3.9'].find((candidate) => run(candidate, ['-c', 'pass']).status === 0);
  if (older) {
    const olderInterpreter = venv(older, 'venv-older', ['pytest']);
    const olderProject = project('fixture-older');
    // The fixture uses `match`, which needs 3.10; on 3.9 measure a file
    // without it.
    const suite = run(older, ['-c', 'import sys; print(sys.version_info >= (3, 10))']).stdout.trim() === 'True' ? 'tests' : 'tests/test_unittest_style.py';
    const result = supercov(olderProject, 'probes', ['--', olderInterpreter, '-m', 'pytest', '-q', '-p', 'no:cacheprovider', suite], { PATH: `${resolve(olderInterpreter, '..')}${delimiter}${process.env.PATH}` });
    ok(result, `probes on ${older}`);
    assert.match(result.stderr, /Python coverage: \d+ test\(s\)/, `${older}: measured through probes`);
    const olderSummary = summary(olderProject, latestRun(olderProject));
    assert.ok(olderSummary.lines[0] > 0, `${older}: lines covered`);
    console.log(`[python-probes] ${older}: ${olderSummary.lines.join('/')} lines`);
  } else {
    console.log('[python-probes] no CPython 3.9-3.11 on PATH; the older-interpreter case was not run');
  }

  console.log(`[python-probes] probes agree with monitoring on ${locations.length} decisions, lines ${probes.lines.join('/')}, branches ${probes.branches.join('/')}, conditions ${probes.conditions.join('/')}`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
