#!/usr/bin/env node
// Publication benchmark for a moderately wide Python suite. It is separate
// from the conformance gate because wall-clock budgets vary across CI hosts.

import assert from 'node:assert/strict';
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, resolve } from 'node:path';
import { performance } from 'node:perf_hooks';
import { spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-python-benchmark-'));
const project = resolve(temporary, 'project');
const venv = resolve(temporary, 'venv');
const python = process.env.SUPERCOV_PYTHON ?? 'python3';
const moduleCount = 200;
const testCount = 1000;

function run(program, args, options = {}) {
  const started = performance.now();
  const result = spawnSync(program, args, { encoding: 'utf8', ...options });
  const elapsedMs = Math.round((performance.now() - started) * 10) / 10;
  assert.equal(result.status, 0, `${program} ${args.join(' ')}\n${result.stdout}\n${result.stderr}`);
  return { ...result, elapsedMs };
}

try {
  mkdirSync(resolve(project, 'src'), { recursive: true });
  mkdirSync(resolve(project, 'tests'), { recursive: true });
  writeFileSync(resolve(project, 'src/__init__.py'), '');
  for (let index = 0; index < moduleCount; index += 1) {
    writeFileSync(
      resolve(project, `src/mod_${index.toString().padStart(3, '0')}.py`),
      'def classify(a, b):\n    if a and b:\n        return 1\n    return 0\n',
    );
  }
  const imports = Array.from(
    { length: moduleCount },
    (_, index) => `from src import mod_${index.toString().padStart(3, '0')}`,
  ).join('\n');
  const modules = Array.from(
    { length: moduleCount },
    (_, index) => `mod_${index.toString().padStart(3, '0')}`,
  ).join(', ');
  writeFileSync(
    resolve(project, 'tests/test_many.py'),
    `${imports}\n\nMODULES = [${modules}]\n\ndef make_test(index):\n    def test():\n        assert MODULES[index % len(MODULES)].classify(True, index % 2 == 0) == int(index % 2 == 0)\n    return test\n\nfor index in range(${testCount}):\n    globals()[f"test_{index:04}"] = make_test(index)\n`,
  );
  writeFileSync(
    resolve(project, 'pyproject.toml'),
    '[tool.pytest.ini_options]\naddopts = "-p no:cacheprovider"\n',
  );
  run('git', ['init', '-q', '.'], { cwd: project });
  run(python, ['-m', 'venv', venv]);
  const venvPython = resolve(venv, process.platform === 'win32' ? 'Scripts/python.exe' : 'bin/python');
  run(venvPython, ['-m', 'pip', 'install', '--disable-pip-version-check', '-q', 'pytest']);
  const environment = {
    ...process.env,
    PATH: `${resolve(venv, process.platform === 'win32' ? 'Scripts' : 'bin')}${delimiter}${process.env.PATH}`,
    SUPERCOV_RUST_BINARY: resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`),
    SUPERCOV_VERBOSE: '1',
  };
  const plain = run(venvPython, ['-m', 'pytest', '-q'], { cwd: project, env: environment });
  const measured = run(
    process.execPath,
    [resolve(repository, 'bin/supercov.js'), '--', venvPython, '-m', 'pytest', '-q'],
    { cwd: project, env: environment },
  );
  assert.match(measured.stderr, new RegExp(`${testCount} test\\(s\\)`));
  const timings = measured.stderr.match(
    /python evidence: join=([0-9.]+)ms serialize=([0-9.]+)ms archive=([0-9.]+)ms/,
  );
  assert(timings, measured.stderr);
  const publication = timings.slice(1).map(Number);
  console.log(JSON.stringify({
    python,
    modules: moduleCount,
    tests: testCount,
    plainMs: plain.elapsedMs,
    supercovMs: measured.elapsedMs,
    overheadMs: Math.round((measured.elapsedMs - plain.elapsedMs) * 10) / 10,
    publicationMs: {
      join: publication[0],
      serialize: publication[1],
      archive: publication[2],
      total: Math.round(publication.reduce((sum, value) => sum + value, 0) * 10) / 10,
    },
  }, null, 2));
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
