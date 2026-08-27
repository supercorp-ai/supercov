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
import {tmpdir} from 'node:os';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const supercov = join(root, 'target/debug/supercov');
const scratch = mkdtempSync(join(tmpdir(), 'supercov-cargo-runner-'));
const workspace = join(scratch, 'workspace');

function packageSource(name, fail) {
  return [
    '#[test]',
    'fn environment_and_order() {',
    `    assert_eq!(std::env::var("SUPERCOV_BUILD_VALUE").as_deref(), Ok("${name}"));`,
    `    assert!(std::env::var("CARGO_MANIFEST_DIR").unwrap().ends_with("/${name}"));`,
    '    let marker = std::path::Path::new(&std::env::var("SUPERCOV_MARKER_ROOT").unwrap())',
    `        .join("${name}");`,
    '    std::fs::write(marker, b"ran").unwrap();',
    ...(fail ? ['    panic!("deliberate package failure");'] : []),
    '}',
    '',
  ].join('\n');
}

function writeWorkspace() {
  mkdirSync(workspace, {recursive: true});
  writeFileSync(
    join(workspace, 'Cargo.toml'),
    '[workspace]\nmembers=["a", "b", "c"]\nresolver="2"\n',
  );
  for (const name of ['a', 'b', 'c']) {
    const packageRoot = join(workspace, name);
    mkdirSync(join(packageRoot, 'src'), {recursive: true});
    writeFileSync(
      join(packageRoot, 'Cargo.toml'),
      [
        '[package]',
        `name="runner-${name}"`,
        'version="0.0.0"',
        'edition="2024"',
        'build="build.rs"',
        '',
        '[lib]',
        'name="shared_target"',
        '',
      ].join('\n'),
    );
    writeFileSync(
      join(packageRoot, 'build.rs'),
      `fn main() { println!("cargo:rustc-env=SUPERCOV_BUILD_VALUE=${name}"); }\n`,
    );
    writeFileSync(join(packageRoot, 'src/lib.rs'), packageSource(name, name === 'b'));
  }
}

function runCase(runId, noFailFast) {
  const runRoot = join(workspace, '.supercov/work', runId);
  const target = join(runRoot, 'rust-target');
  const output = join(runRoot, 'rust-compiler/cargo-runner');
  const markers = join(runRoot, 'markers');
  mkdirSync(target, {recursive: true});
  mkdirSync(output, {recursive: true});
  mkdirSync(markers, {recursive: true});
  const configPath = join(runRoot, 'cargo-runner.json');
  writeFileSync(
    configPath,
    JSON.stringify({version: 1, runId, targetDirectory: target, outputDirectory: output}),
    {flag: 'wx', mode: 0o600},
  );
  const runner = `target.'cfg(all())'.runner=[${JSON.stringify(supercov)},"__cargo-test-runner"]`;
  const args = ['test', '--workspace', '--lib'];
  if (noFailFast) args.push('--no-fail-fast');
  args.push('--config', runner);
  const result = spawnSync('cargo', args, {
    cwd: workspace,
    encoding: 'utf8',
    timeout: 120_000,
    killSignal: 'SIGKILL',
    env: {
      ...process.env,
      CARGO_TARGET_DIR: target,
      SUPERCOV_RUST_CARGO_RUNNER_CONFIG: configPath,
      SUPERCOV_MARKER_ROOT: markers,
    },
  });
  if (result.error) throw result.error;
  assert.notEqual(result.status, 0, 'the deliberate package failure was lost');
  const units = readdirSync(output)
    .filter((name) => name.startsWith('libtest-') && name.endsWith('.json'))
    .sort()
    .map((name) => JSON.parse(readFileSync(join(output, name), 'utf8')));
  return {markers, result, units};
}

try {
  writeWorkspace();
  const failFast = runCase('run_70123456789abcde', false);
  assert(existsSync(join(failFast.markers, 'a')));
  assert(existsSync(join(failFast.markers, 'b')));
  assert(!existsSync(join(failFast.markers, 'c')));
  assert.equal(failFast.units.length, 2);
  assert.deepEqual(
    failFast.units.map(({invocationOrdinal}) => invocationOrdinal).sort(),
    [0, 1],
  );
  assert.deepEqual(
    failFast.units.flatMap(({attempts}) => attempts.map(({test}) => test)),
    ['environment_and_order', 'environment_and_order'],
  );
  assert.deepEqual(
    failFast.units.flatMap(({attempts}) => attempts.map(({result}) => result.status)).sort(),
    [0, 101],
  );

  const noFailFast = runCase('run_80123456789abcde', true);
  assert(existsSync(join(noFailFast.markers, 'a')));
  assert(existsSync(join(noFailFast.markers, 'b')));
  assert(existsSync(join(noFailFast.markers, 'c')));
  assert.equal(noFailFast.units.length, 3);
  assert.deepEqual(
    noFailFast.units.map(({invocationOrdinal}) => invocationOrdinal).sort(),
    [0, 1, 2],
  );
  assert.deepEqual(
    noFailFast.units.flatMap(({attempts}) => attempts.map(({result}) => result.status)).sort(),
    [0, 0, 101],
  );
  console.log(
    '[rust-cargo-runner] Cargo-owned package/build-script environment, artifact order, default fail-fast, and --no-fail-fast are preserved',
  );
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
