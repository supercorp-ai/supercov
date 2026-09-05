#!/usr/bin/env node
// End-to-end conformance gate for the owned Python frontend. A real supported
// CPython runs the public CLI through serial pytest, xdist, reruns, a killed
// worker, concurrency adapters, unittest and the instruction-position corpus.
// The coverage.py fixture remains an independent line/branch-outcome oracle;
// coverage.py itself is never imported by the product path.

import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, delimiter, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const launcher = resolve(repository, 'bin/supercov.js');
const monitoringFixture = resolve(repository, 'tests/fixtures/python-monitoring');
const positionFixture = resolve(repository, 'tests/fixtures/python-position-corpus');
const oracleFixture = resolve(repository, 'tests/fixtures/python-pytest');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-python-monitoring-'));

function interpreterVersion(program) {
  const probe = spawnSync(program, ['-c', 'import sys; print(sys.version_info[0], sys.version_info[1])'], {
    encoding: 'utf8',
  });
  if (probe.status !== 0) return null;
  const [major, minor] = probe.stdout.trim().split(' ').map(Number);
  return { major, minor };
}

function findInterpreter() {
  const candidates = process.env.SUPERCOV_PYTHON
    ? [process.env.SUPERCOV_PYTHON]
    : ['python3.14', 'python3.13', 'python3.12', 'python3', 'python'];
  for (const candidate of candidates) {
    const version = interpreterVersion(candidate);
    if (version && version.major === 3 && version.minor >= 12) return candidate;
  }
  throw new Error(
    'python-monitoring integration needs CPython 3.12 or newer on PATH (or SUPERCOV_PYTHON)',
  );
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  assert.equal(result.status, 0, `${program} ${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  return result;
}

function createProject(source, name) {
  const project = resolve(temporary, name);
  cpSync(source, project, { recursive: true });
  run('git', ['init', '-q', '.'], { cwd: project });
  return project;
}

function environmentFor(project, venv) {
  const environment = {
    ...process.env,
    SUPERCOV_RUST_BINARY: binary,
    PATH: `${resolve(venv, process.platform === 'win32' ? 'Scripts' : 'bin')}${delimiter}${process.env.PATH}`,
    SUPERCOV_PROJECT_ROOT: project,
  };
  delete environment.PYTHONPATH;
  delete environment.PYTEST_PLUGINS;
  return environment;
}

function supercov(project, args, environment) {
  return spawnSync(process.execPath, [launcher, ...args], {
    cwd: project,
    encoding: 'utf8',
    env: environment,
  });
}

function successfulSupercov(project, args, environment) {
  const result = supercov(project, args, environment);
  assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}`);
  return result;
}

function query(project, args, environment) {
  const result = successfulSupercov(project, [...args, '--json'], environment);
  const payload = JSON.parse(result.stdout);
  assert.equal(payload.ok, true, result.stdout);
  return payload.data;
}

function decisionVectors(project, location, environment, filter = null) {
  const args = ['runs', 'latest', 'decision', location];
  if (filter) args.push('--filter', filter);
  const decision = query(project, args, environment).decisions[0];
  return decision.vectors
    .map((vector) => `${vector.values.map((value) => (value === null ? '-' : value ? 'T' : 'F')).join('')}->${vector.outcome ? 'T' : 'F'}`)
    .sort();
}

function assertFixtureTotals(summary) {
  assert.equal(summary.model.variant, 'python-owned-monitoring');
  assert.deepEqual(
    [summary.coverage.lines.covered, summary.coverage.lines.total],
    [81, 82],
    JSON.stringify(summary.coverage),
  );
  assert.deepEqual(
    [summary.coverage.branches.covered, summary.coverage.branches.total],
    [79, 94],
    JSON.stringify(summary.coverage),
  );
  assert.deepEqual(
    [summary.coverage.coveredConditions, summary.coverage.conditions],
    [8, 16],
    JSON.stringify(summary.coverage),
  );
  assert.equal(summary.testExitCode, 0);
}

function assertOracleAgreement(project, environment) {
  const expectedCoveredLines = new Set([1, 2, 3, 4, 5, 9, 10, 11, 12, 14]);
  const expectedMissingLines = new Set([6, 13]);
  for (const line of [...expectedCoveredLines, ...expectedMissingLines]) {
    const detail = query(project, ['runs', 'latest', 'line', `src/calculator.py:${line}`], environment);
    assert.equal(
      detail.covered,
      expectedCoveredLines.has(line),
      `coverage.py line differential disagreed at calculator.py:${line}`,
    );
  }
  const expectedMissingOutcomes = new Map([[2, 0], [4, 1], [10, 0], [12, 1]]);
  for (const [line, expected] of expectedMissingOutcomes) {
    const detail = query(project, ['runs', 'latest', 'line', `src/calculator.py:${line}`], environment);
    const missing = detail.remaining.filter((obligation) => obligation.kind === 'branch').length;
    assert.equal(missing, expected, `coverage.py branch differential disagreed at calculator.py:${line}`);
  }
}

try {
  const python = findInterpreter();
  const venv = resolve(temporary, 'venv');
  run(python, ['-m', 'venv', venv]);
  const venvPython = resolve(venv, process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python');
  run(venvPython, [
    '-m', 'pip', 'install', '--disable-pip-version-check', '-q',
    'pytest', 'pytest-xdist', 'pytest-rerunfailures',
  ]);

  const project = createProject(monitoringFixture, 'monitoring');
  const environment = environmentFor(project, venv);

  const serial = successfulSupercov(
    project,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests'],
    environment,
  );
  assert.match(serial.stdout, /\[coverage\] evidence:/);
  assert.match(serial.stderr, /14 test\(s\) across 2 source file\(s\)/);
  assert.match(serial.stderr, /interpreter process\(es\) on Python 3\./);
  assertFixtureTotals(query(project, ['runs', 'latest'], environment));
  assert.deepEqual(
    decisionVectors(project, 'app/shapes.py:26', environment),
    ['TF->T', 'TT->F'],
    'not (a and b) keeps both operands as conditions',
  );

  const comprehension = query(project, ['runs', 'latest', 'decision', 'app/shapes.py:12'], environment);
  assert.equal(comprehension.decisions[0].executed, true, 'comprehension filter mapped by offset order');
  const subprocessLine = query(project, ['runs', 'latest', 'line', 'app/shapes.py:34'], environment);
  assert.match(JSON.stringify(subprocessLine), /test_thread_and_subprocess/, 'child interpreter inherits exact identity');
  const exceptions = query(project, ['runs', 'latest', 'line', 'app/shapes.py:93'], environment);
  assert.doesNotMatch(JSON.stringify(exceptions), /not observed: try completed/, 'try completion is structural');

  successfulSupercov(
    project,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', '-n', '2', 'tests'],
    environment,
  );
  assertFixtureTotals(query(project, ['runs', 'latest'], environment));

  const rerun = successfulSupercov(
    project,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests_extended/test_rerun.py'],
    environment,
  );
  assert.match(rerun.stderr, /1 test\(s\) across 2 source file\(s\)/, 'retry count is one logical test');
  const rerunSummary = query(project, ['runs', 'latest'], environment);
  assert.equal(rerunSummary.tests, 1);
  assert.equal(rerunSummary.testOutcomes.flaky, 1);
  assert.deepEqual(decisionVectors(project, 'app/shapes.py:26', environment, 'failed'), ['TF->T']);
  assert.deepEqual(decisionVectors(project, 'app/shapes.py:26', environment, 'passed'), ['TT->F']);

  const crashEnvironment = {
    ...environment,
    SUPERCOV_PYTHON_CRASH_MARKER: resolve(temporary, 'worker-crashed'),
  };
  successfulSupercov(
    project,
    [
      '--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider',
      '-n', '1', '--reruns', '1', 'tests_extended/test_crash.py',
    ],
    crashEnvironment,
  );
  const crashSummary = query(project, ['runs', 'latest'], crashEnvironment);
  assert.equal(crashSummary.tests, 1);
  assert.equal(crashSummary.testOutcomes.flaky, 1);
  assert.deepEqual(
    decisionVectors(project, 'app/shapes.py:26', crashEnvironment),
    ['TF->T', 'TT->F'],
    'mmap retains the killed worker decision before os._exit',
  );
  assert.deepEqual(decisionVectors(project, 'app/shapes.py:26', crashEnvironment, 'failed'), ['TF->T']);

  successfulSupercov(
    project,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests_extended/test_concurrency.py'],
    environment,
  );
  const smallLine = query(project, ['runs', 'latest', 'line', 'app/shapes.py:35'], environment);
  assert.match(JSON.stringify(smallLine), /test_thread_pool_first_context/);
  const largeLine = query(project, ['runs', 'latest', 'line', 'app/shapes.py:37'], environment);
  const largeOwners = JSON.stringify(largeLine);
  assert.match(largeOwners, /test_reused_thread_pool_gets_new_context/);
  assert.match(largeOwners, /test_spawned_multiprocessing_context/);
  assert.match(largeOwners, /test_interleaved_asyncio_tasks_keep_one_test_context/);

  successfulSupercov(
    project,
    [
      '--', 'python', '-m', 'unittest', '-q',
      'tests_extended.test_unittest_subtests.SubTestCases.test_passing_subtests',
    ],
    environment,
  );
  const subtestLine = query(project, ['runs', 'latest', 'line', 'app/shapes.py:35'], environment);
  assert.match(JSON.stringify(subtestLine), /SubTestCases\.test_passing_subtests/);
  const failedSubtest = supercov(
    project,
    [
      '--', 'python', '-m', 'unittest', '-q',
      'tests_extended.test_unittest_subtests.SubTestCases.test_failing_subtest_rolls_up',
    ],
    environment,
  );
  assert.equal(failedSubtest.status, 1, `${failedSubtest.stdout}\n${failedSubtest.stderr}`);
  const failedSubtestSummary = query(project, ['runs', 'latest'], environment);
  assert.equal(failedSubtestSummary.testOutcomes.failed, 1);

  successfulSupercov(
    project,
    [
      '--', 'python', '-X', 'no_debug_ranges', '-m', 'pytest', '-q',
      '-p', 'no:cacheprovider', 'tests/test_shapes.py::test_chained',
    ],
    environment,
  );
  const noDebug = query(project, ['runs', 'latest'], environment);
  assert(noDebug.filesWithMeasurementLimitations > 0, 'missing positions must be a blocking limitation');
  const limitedLine = query(project, ['runs', 'latest', 'line', 'app/shapes.py:34'], environment);
  assert.equal(limitedLine.totalRemaining, 0, 'missing positions must remove the line from the measured denominator');

  const isolated = supercov(
    project,
    [
      '--', 'python', '-I', '-m', 'pytest', '-q', '-p', 'no:cacheprovider',
      'tests/test_shapes.py::test_chained',
    ],
    environment,
  );
  assert.notEqual(isolated.status, 0, 'isolated mode must fail closed instead of publishing partial evidence');
  assert.doesNotMatch(isolated.stdout, /\[coverage\] evidence:/);

  const positionProject = createProject(positionFixture, 'positions');
  const positionEnvironment = environmentFor(positionProject, venv);
  successfulSupercov(
    positionProject,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests'],
    positionEnvironment,
  );
  const positionSummary = query(positionProject, ['runs', 'latest'], positionEnvironment);
  const positionFile = query(
    positionProject,
    ['runs', 'latest', 'file', 'src/corpus.py', '--limit', '200'],
    positionEnvironment,
  );
  assert.equal(
    positionSummary.filesWithMeasurementLimitations,
    0,
    `position corpus must map completely: ${JSON.stringify(positionFile.gapLines.filter((line) => line.limitations.length))}`,
  );
  assert.equal(positionSummary.testExitCode, 0);
  assert.deepEqual(
    [
      positionSummary.coverage.lines.covered,
      positionSummary.coverage.lines.total,
      positionSummary.coverage.statements.covered,
      positionSummary.coverage.statements.total,
      positionSummary.coverage.functions.covered,
      positionSummary.coverage.functions.total,
      positionSummary.coverage.branches.covered,
      positionSummary.coverage.branches.total,
      positionSummary.coverage.coveredConditions,
      positionSummary.coverage.conditions,
    ],
    [67, 67, 69, 69, 17, 17, 94, 114, 12, 27],
    'position corpus must have identical gaps on every supported interpreter',
  );

  const oracleProject = resolve(temporary, 'oracle');
  mkdirSync(oracleProject, { recursive: true });
  cpSync(resolve(oracleFixture, 'src'), resolve(oracleProject, 'src'), { recursive: true });
  cpSync(resolve(oracleFixture, 'tests'), resolve(oracleProject, 'tests'), { recursive: true });
  run('git', ['init', '-q', '.'], { cwd: oracleProject });
  const oracleEnvironment = environmentFor(oracleProject, venv);
  successfulSupercov(
    oracleProject,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'tests'],
    oracleEnvironment,
  );
  assertOracleAgreement(oracleProject, oracleEnvironment);


  // A suite stops a server it started by signalling it. Python's default
  // SIGTERM ends the process without running atexit, so nothing written at
  // exit would survive -- but evidence goes into an mmap that the kernel has
  // already made durable, which is what makes a killed worker's coverage
  // recoverable at all. Assert that directly, for SIGKILL as well as SIGTERM,
  // because any move back to a buffer flushed at exit would silently undo it.
  const signalProject = resolve(temporary, 'signals');
  mkdirSync(signalProject, { recursive: true });
  writeFileSync(
    resolve(signalProject, 'worker.py'),
    [
      'def handle(kind):',
      '    if kind == "termed":',
      '        first = len(kind)',
      '        return "termed%d" % first',
      '    if kind == "killed":',
      '        second = len(kind)',
      '        return "killed%d" % second',
      '    return "other"',
      '',
    ].join('\n'),
  );
  writeFileSync(
    resolve(signalProject, 'child.py'),
    [
      'import sys',
      'from worker import handle',
      '',
      'for line in sys.stdin:',
      '    sys.stdout.write(handle(line.strip()) + "\\n")',
      '    sys.stdout.flush()',
      '',
    ].join('\n'),
  );
  writeFileSync(
    resolve(signalProject, 'test_signal.py'),
    [
      'import os',
      'import signal',
      'import subprocess',
      'import sys',
      '',
      '',
      'def drive(word, number):',
      '    child = subprocess.Popen(',
      '        [sys.executable, "child.py"],',
      '        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True,',
      '    )',
      '    child.stdin.write(word + "\\n")',
      '    child.stdin.flush()',
      '    assert word in child.stdout.readline()',
      '    os.kill(child.pid, number)',
      '    child.wait()',
      '    assert child.returncode == -number, "the child must die from the signal"',
      '',
      '',
      'def test_terminated():',
      '    drive("termed", signal.SIGTERM)',
      '',
      '',
      'def test_hard_killed():',
      '    drive("killed", signal.SIGKILL)',
      '',
    ].join('\n'),
  );
  const signalEnvironment = environmentFor(signalProject, venv);
  successfulSupercov(
    signalProject,
    ['--', 'python', '-m', 'pytest', '-q', '-p', 'no:cacheprovider', 'test_signal.py'],
    signalEnvironment,
  );
  for (const [line, test] of [[3, 'test_terminated'], [6, 'test_hard_killed']]) {
    const detail = query(signalProject, ['runs', 'latest', 'line', `worker.py:${line}`], signalEnvironment);
    assert.match(
      JSON.stringify(detail),
      new RegExp(test),
      `worker.py:${line} must keep the coverage its signalled child produced`,
    );
  }

  console.log(
    `[python-monitoring] ${basename(python)} passed serial, xdist, retry, crash, concurrency, unittest, positions, signalled children and oracle differentials`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
