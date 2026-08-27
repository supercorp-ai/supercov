import assert from 'node:assert/strict';
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const supercov = join(root, 'target/debug/supercov');
const nextest = process.env.SUPERCOV_NEXTEST_BIN;
const nextestVersion = process.env.SUPERCOV_NEXTEST_VERSION ?? '0.9.140';
if (!['0.9.138', '0.9.140'].includes(nextestVersion)) {
  throw new Error(
    `unsupported focused nextest contract version: ${nextestVersion}`,
  );
}
if (!nextest || !existsSync(nextest)) {
  throw new Error(
    `SUPERCOV_NEXTEST_BIN must name cargo-nextest ${nextestVersion}`,
  );
}
const version = spawnSync(nextest, ['nextest', '--version'], {
  encoding: 'utf8',
});
if (
  version.status !== 0 ||
  !version.stdout.startsWith(`cargo-nextest ${nextestVersion} `)
) {
  throw new Error(
    `expected cargo-nextest ${nextestVersion}, received: ${version.stdout}${version.stderr}`,
  );
}

const scratch = mkdtempSync(join(tmpdir(), 'supercov-nextest-runner-'));
const workspace = join(scratch, 'workspace');
const runId = 'run_90123456789abcde';
const runRoot = join(workspace, '.supercov/work', runId);
const target = join(runRoot, 'rust-target');
const output = join(runRoot, 'rust-compiler/cargo-runner');

try {
  mkdirSync(join(workspace, 'src'), { recursive: true });
  mkdirSync(target, { recursive: true });
  mkdirSync(output, { recursive: true });
  writeFileSync(
    join(workspace, 'Cargo.toml'),
    '[package]\nname="nextest-retry-fixture"\nversion="0.0.0"\nedition="2024"\n',
  );
  writeFileSync(
    join(workspace, 'src/lib.rs'),
    [
      '#[cfg(test)]',
      'mod tests {',
      '    #[test]',
      '    fn fails_once_then_passes() {',
      '        assert_ne!(std::env::var("NEXTEST_ATTEMPT").as_deref(), Ok("1"));',
      '    }',
      '',
      '    fn wait_for_peer(name: &str, peer: &str) {',
      '        let root = std::path::PathBuf::from(std::env::var_os("SUPERCOV_NEXTEST_CONCURRENCY_DIR").unwrap());',
      '        std::fs::write(root.join(name), b"ready").unwrap();',
      '        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);',
      '        while !root.join(peer).is_file() {',
      '            assert!(std::time::Instant::now() < deadline, "nextest attempts did not overlap");',
      '            std::thread::sleep(std::time::Duration::from_millis(10));',
      '        }',
      '        std::thread::sleep(std::time::Duration::from_millis(100));',
      '    }',
      '',
      '    #[test]',
      '    fn concurrent_a() {',
      '        wait_for_peer("a", "b");',
      '    }',
      '',
      '    #[test]',
      '    fn concurrent_b() {',
      '        wait_for_peer("b", "a");',
      '    }',
      '',
      '    #[cfg(unix)]',
      '    #[test]',
      '    fn kills_supercov_runner() {',
      '        unsafe extern "C" {',
      '            fn getppid() -> i32;',
      '            fn kill(pid: i32, signal: i32) -> i32;',
      '        }',
      '        std::fs::write(std::env::var_os("SUPERCOV_NEXTEST_CRASH_PID").unwrap(), std::process::id().to_string()).unwrap();',
      '        let parent = unsafe { getppid() };',
      '        assert_eq!(unsafe { kill(parent, 9) }, 0);',
      '        std::thread::sleep(std::time::Duration::from_secs(30));',
      '        panic!("the target-runner watchdog did not contain the test process");',
      '    }',
      '}',
      '',
    ].join('\n'),
  );
  const rustcVersion = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
  if (rustcVersion.status !== 0)
    throw new Error(rustcVersion.stderr || 'rustc -vV failed');
  const targetIdentity = rustcVersion.stdout.match(/^host: (.+)$/m)?.[1];
  if (!targetIdentity)
    throw new Error('rustc -vV did not report a host target');
  const configPath = join(runRoot, 'cargo-runner.json');
  writeFileSync(
    configPath,
    JSON.stringify({
      version: 3,
      runId,
      targetDirectory: target,
      outputDirectory: output,
      targetRunners: [{ target: targetIdentity, underlyingRunner: null }],
    }),
    { flag: 'wx', mode: 0o600 },
  );
  const runner = `target.${JSON.stringify(targetIdentity)}.runner=[${JSON.stringify(supercov)},"__cargo-test-runner",${JSON.stringify(targetIdentity)}]`;
  const result = spawnSync(
    nextest,
    [
      'nextest',
      'run',
      '--retries',
      '1',
      '--config',
      runner,
      '--',
      '--exact',
      'tests::fails_once_then_passes',
    ],
    {
      cwd: workspace,
      encoding: 'utf8',
      timeout: 120_000,
      killSignal: 'SIGKILL',
      env: {
        ...process.env,
        CARGO_TARGET_DIR: target,
        SUPERCOV_RUST_CARGO_RUNNER_CONFIG: configPath,
      },
    },
  );
  if (result.error) throw result.error;
  assert.equal(result.status, 0, result.stderr || result.stdout);
  assert.match(result.stdout + result.stderr, /1 flaky/u);

  const names = readdirSync(output).sort();
  const units = names
    .filter((name) => name.startsWith('libtest-') && name.endsWith('.json'))
    .map((name) => JSON.parse(readFileSync(join(output, name), 'utf8')));
  assert.equal(
    units.length,
    2,
    `expected two attempt units, found ${names.join(', ')}`,
  );
  assert.deepEqual(
    units.map(({ runner }) => runner),
    ['nextest', 'nextest'],
  );
  assert.equal(new Set(units.map(({ runnerRunId }) => runnerRunId)).size, 1);
  assert.deepEqual(
    units.map(({ runnerVersion }) => runnerVersion),
    [nextestVersion, nextestVersion],
  );
  assert.deepEqual(
    units.map(({ runnerBinaryId }) => runnerBinaryId),
    ['nextest-retry-fixture', 'nextest-retry-fixture'],
  );
  const attempts = units
    .flatMap(({ attempts }) => attempts)
    .sort((a, b) => a.retry - b.retry);
  assert.deepEqual(
    attempts.map(({ retry }) => retry),
    [0, 1],
  );
  assert.deepEqual(
    attempts.map(({ totalAttempts }) => totalAttempts),
    [2, 2],
  );
  assert.deepEqual(
    attempts.map(({ result: attemptResult }) => attemptResult.status),
    [101, 0],
  );
  assert.equal(
    new Set(attempts.map(({ runnerAttemptId }) => runnerAttemptId)).size,
    2,
  );
  assert(
    names.filter(
      (name) => name.startsWith('.sequence-') && name.endsWith('.reserved'),
    ).length === 2,
  );

  const concurrentRunId = 'run_90123456789abcdf';
  const concurrentOutput = join(
    runRoot,
    'rust-compiler/cargo-runner-concurrent',
  );
  const concurrentConfig = join(runRoot, 'cargo-runner-concurrent.json');
  const concurrencyDirectory = join(runRoot, 'concurrent-barrier');
  mkdirSync(concurrentOutput, { recursive: true });
  mkdirSync(concurrencyDirectory, { recursive: true });
  writeFileSync(
    concurrentConfig,
    JSON.stringify({
      version: 3,
      runId: concurrentRunId,
      targetDirectory: target,
      outputDirectory: concurrentOutput,
      targetRunners: [{ target: targetIdentity, underlyingRunner: null }],
    }),
    { flag: 'wx', mode: 0o600 },
  );
  const concurrentResult = spawnSync(
    nextest,
    [
      'nextest',
      'run',
      '--test-threads',
      '2',
      '--config',
      runner,
      '-E',
      'test(/concurrent_/)',
    ],
    {
      cwd: workspace,
      encoding: 'utf8',
      timeout: 120_000,
      killSignal: 'SIGKILL',
      env: {
        ...process.env,
        CARGO_TARGET_DIR: target,
        SUPERCOV_NEXTEST_CONCURRENCY_DIR: concurrencyDirectory,
        SUPERCOV_RUST_CARGO_RUNNER_CONFIG: concurrentConfig,
      },
    },
  );
  if (concurrentResult.error) throw concurrentResult.error;
  assert.equal(
    concurrentResult.status,
    0,
    concurrentResult.stderr || concurrentResult.stdout,
  );
  const concurrentNames = readdirSync(concurrentOutput).sort();
  const concurrentUnits = concurrentNames
    .filter((name) => name.startsWith('libtest-') && name.endsWith('.json'))
    .map((name) =>
      JSON.parse(readFileSync(join(concurrentOutput, name), 'utf8')),
    );
  assert.equal(concurrentUnits.length, 2);
  assert.equal(
    new Set(concurrentUnits.map(({ invocationOrdinal }) => invocationOrdinal))
      .size,
    2,
  );
  assert.equal(
    new Set(concurrentUnits.map(({ runnerRunId }) => runnerRunId)).size,
    1,
  );
  assert.notEqual(concurrentUnits[0].runnerRunId, units[0].runnerRunId);
  const concurrentAttempts = concurrentUnits.flatMap(
    ({ attempts: value }) => value,
  );
  assert.deepEqual(concurrentAttempts.map(({ test }) => test).sort(), [
    'tests::concurrent_a',
    'tests::concurrent_b',
  ]);
  assert.equal(
    new Set(concurrentAttempts.map(({ runnerAttemptId }) => runnerAttemptId))
      .size,
    2,
  );
  assert(
    Math.max(...concurrentAttempts.map(({ startedAtMs }) => startedAtMs)) <=
      Math.min(...concurrentAttempts.map(({ endedAtMs }) => endedAtMs)),
    'concurrent nextest attempts did not retain overlapping execution intervals',
  );
  assert.equal(
    concurrentNames.filter(
      (name) => name.startsWith('.sequence-') && name.endsWith('.reserved'),
    ).length,
    2,
  );

  if (process.platform !== 'win32') {
    const crashRunId = 'run_90123456789abcd0';
    const crashOutput = join(runRoot, 'rust-compiler/cargo-runner-crash');
    const crashConfig = join(runRoot, 'cargo-runner-crash.json');
    const crashPid = join(runRoot, 'crash-test.pid');
    mkdirSync(crashOutput, { recursive: true });
    writeFileSync(
      crashConfig,
      JSON.stringify({
        version: 3,
        runId: crashRunId,
        targetDirectory: target,
        outputDirectory: crashOutput,
        targetRunners: [{ target: targetIdentity, underlyingRunner: null }],
      }),
      { flag: 'wx', mode: 0o600 },
    );
    const crashResult = spawnSync(
      nextest,
      ['nextest', 'run', '--config', runner, 'kills_supercov_runner'],
      {
        cwd: workspace,
        encoding: 'utf8',
        timeout: 120_000,
        killSignal: 'SIGKILL',
        env: {
          ...process.env,
          CARGO_TARGET_DIR: target,
          SUPERCOV_NEXTEST_CRASH_PID: crashPid,
          SUPERCOV_RUST_CARGO_RUNNER_CONFIG: crashConfig,
        },
      },
    );
    if (crashResult.error) throw crashResult.error;
    assert.equal(
      crashResult.status,
      100,
      crashResult.stderr || crashResult.stdout,
    );
    const crashNames = readdirSync(crashOutput).sort();
    assert.equal(
      crashNames.filter(
        (name) => name.startsWith('.sequence-') && name.endsWith('.reserved'),
      ).length,
      1,
      'an uncatchable target-runner death lost its durable invocation reservation',
    );
    assert.equal(
      crashNames.filter(
        (name) => name.startsWith('libtest-') && name.endsWith('.json'),
      ).length,
      0,
      'an uncatchable target-runner death published a false complete unit',
    );
    const childPid = Number.parseInt(readFileSync(crashPid, 'utf8'), 10);
    assert(Number.isSafeInteger(childPid) && childPid > 1);
    let childAlive = true;
    const deadline = Date.now() + 5_000;
    while (childAlive && Date.now() < deadline) {
      try {
        process.kill(childPid, 0);
      } catch (error) {
        if (error?.code !== 'ESRCH') throw error;
        childAlive = false;
      }
      if (childAlive)
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 25);
    }
    assert.equal(
      childAlive,
      false,
      "the target-runner watchdog let the killed runner's test escape",
    );
  }
  console.log(
    '[rust-nextest-runner] list, retries and concurrent attempts retain exact identity; uncatchable runner death is durable and contained',
  );
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
