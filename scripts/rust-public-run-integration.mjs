import assert from 'node:assert/strict';
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { once } from 'node:events';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { spawn, spawnSync } from 'node:child_process';

const repository = resolve(import.meta.dirname, '..');
const binary = resolve(repository, 'target/debug/supercov');
const launcher = resolve(repository, 'bin/supercov.js');
const fixture = resolve(repository, 'tests/fixtures/generic-node');
const temporary = mkdtempSync(resolve(tmpdir(), 'supercov-rust-public-run-'));
const project = resolve(temporary, 'project');

function run(arguments_) {
  return spawnSync(process.execPath, [launcher, ...arguments_], {
    cwd: project,
    encoding: 'utf8',
    env: {
      ...process.env,
      SUPERCOV_RUST_BINARY: binary,
    },
  });
}

function publishedRuns() {
  return readdirSync(resolve(project, '.supercov/runs')).sort();
}

try {
  mkdirSync(project);
  cpSync(resolve(fixture, 'package.json'), resolve(project, 'package.json'));
  cpSync(resolve(fixture, 'src'), resolve(project, 'src'), { recursive: true });
  cpSync(resolve(fixture, 'tests'), resolve(project, 'tests'), { recursive: true });
  mkdirSync(resolve(project, 'node_modules'));
  symlinkSync(resolve(repository, 'node_modules/expect'), resolve(project, 'node_modules/expect'));

  const missing = run(['--']);
  assert.equal(missing.status, 2);
  assert.equal(missing.stderr, 'Usage: supercov -- <test command>\n');

  const successful = run(['--', process.execPath, '--test', '--test-concurrency=2']);
  assert.equal(successful.status, 0, successful.stderr || successful.stdout);
  assert.match(successful.stdout, /tests 4/);
  assert.match(
    successful.stdout,
    /\[coverage\] evidence: .*\/\.supercov\/runs\/2026-\d\d-\d\dT\d\d-\d\d-\d\d-\d\d\dZ\/evidence\.raw\.gz/,
  );
  assert.match(successful.stderr, /\[supercov\] instrumenting isolated workspace /);
  assert.match(successful.stderr, /\[supercov\] attributed \d+ native node:assert call\(s\)/);
  assert.match(successful.stderr, /\[supercov\] running in isolated workspace: .* --test/);
  assert.match(
    successful.stderr,
    /\[supercov\] timings initialization=\d+(?:\.\d)?ms workspace=\d+(?:\.\d)?ms setup=\d+(?:\.\d)?ms build=\d+(?:\.\d)?ms tests=\d+(?:\.\d)?ms evidence=\d+(?:\.\d)?ms total=\d+(?:\.\d)?ms/,
  );
  const [successfulId] = publishedRuns();
  assert.match(successfulId, /^2026-\d\d-\d\dT\d\d-\d\d-\d\d-\d\d\dZ$/);
  const successfulMetadata = JSON.parse(
    readFileSync(resolve(project, '.supercov/runs', successfulId, 'run.json'), 'utf8'),
  );
  assert.equal(successfulMetadata.id, successfulId);
  assert.equal(successfulMetadata.testExitCode, 0);

  writeFileSync(
    resolve(project, 'tests/failure.test.mjs'),
    [
      "import test from 'node:test';",
      "import assert from 'node:assert/strict';",
      "test('intentional failure', () => assert.equal(1, 2));",
      '',
    ].join('\n'),
  );
  const failed = run(['--', process.execPath, '--test', '--test-concurrency=2']);
  assert.equal(failed.status, 1, failed.stderr || failed.stdout);
  assert.match(failed.stdout, /fail 1/);
  assert.match(failed.stdout, /\[coverage\] evidence: /);
  assert.match(failed.stderr, /\[supercov\] timings /);
  const runs = publishedRuns();
  assert.equal(runs.length, 2);
  const failedId = runs.at(-1);
  const failedMetadata = JSON.parse(
    readFileSync(resolve(project, '.supercov/runs', failedId, 'run.json'), 'utf8'),
  );
  assert.equal(failedMetadata.testExitCode, 1);
  assert.equal(
    readFileSync(resolve(project, 'src/permission.mjs'), 'utf8'),
    readFileSync(resolve(fixture, 'src/permission.mjs'), 'utf8'),
  );
  assert.deepEqual(readdirSync(resolve(project, '.supercov/work')), []);

  rmSync(resolve(project, 'tests/failure.test.mjs'));
  writeFileSync(
    resolve(project, 'tests/interrupted.test.mjs'),
    [
      "import test from 'node:test';",
      "import assert from 'node:assert/strict';",
      "import { permission } from '../src/permission.mjs';",
      "test('long-running test', async () => {",
      "  assert.equal(permission(true, false), 'allowed');",
      '  await new Promise(resolve => setTimeout(resolve, 30_000));',
      '});',
      '',
    ].join('\n'),
  );
  const interrupted = spawn(
    process.execPath,
    [launcher, '--', process.execPath, '--test', '--test-concurrency=2'],
    {
      cwd: project,
      env: {
        ...process.env,
        SUPERCOV_RUST_BINARY: binary,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    },
  );
  let interruptedStdout = '';
  let interruptedStderr = '';
  interrupted.stdout.setEncoding('utf8');
  interrupted.stderr.setEncoding('utf8');
  interrupted.stdout.on('data', chunk => {
    interruptedStdout += chunk;
  });
  const running = new Promise(resolveRunning => {
    interrupted.stderr.on('data', chunk => {
      interruptedStderr += chunk;
      if (interruptedStderr.includes('[supercov] running in isolated workspace:'))
        resolveRunning();
    });
  });
  let readinessTimer;
  await Promise.race([
    running,
    new Promise((_, reject) => {
      readinessTimer = setTimeout(
        () => reject(new Error(`interrupted run never started:\n${interruptedStderr}`)),
        5_000,
      );
    }),
  ]);
  clearTimeout(readinessTimer);
  interrupted.kill('SIGINT');
  const [interruptedCode, interruptedSignal] = await once(interrupted, 'exit');
  assert.equal(interruptedCode, 130);
  assert.equal(interruptedSignal, null);
  assert.doesNotMatch(interruptedStdout, /\[coverage\] evidence:/);
  assert.match(interruptedStderr, /\[supercov\] timings /);
  assert.equal(publishedRuns().length, 2);
  assert.equal(
    readdirSync(resolve(project, '.supercov/locks')).includes('active.json'),
    false,
  );
  const interruptedWork = readdirSync(resolve(project, '.supercov/work'));
  assert.equal(interruptedWork.length, 1);
  const interruptedState = JSON.parse(
    readFileSync(
      resolve(project, '.supercov/work', interruptedWork[0], 'state.json'),
      'utf8',
    ),
  );
  assert.equal(interruptedState.status, 'interrupted');
  assert.equal(interruptedState.signal, 'SIGINT');
  assert.equal(
    readFileSync(resolve(project, 'src/permission.mjs'), 'utf8'),
    readFileSync(resolve(fixture, 'src/permission.mjs'), 'utf8'),
  );

  console.log(
    '[rust-public-run] public Rust wrapping preserves success/failure/signal exits, durable evidence, source isolation, cleanup, progress, and timing UX',
  );
} finally {
  rmSync(temporary, {
    recursive: true,
    force: true,
    maxRetries: 10,
    retryDelay: 20,
  });
}
