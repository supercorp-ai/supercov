import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync, readFileSync, readdirSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const manifest = join(root, 'spikes/rustc-backend/Cargo.toml');
const fixture = join(root, 'spikes/rustc-backend/fixture/Cargo.toml');
const wrapper = join(
  root,
  'spikes/rustc-backend/target/debug/supercov-rustc-backend-spike',
);
const scratch = mkdtempSync(join(tmpdir(), 'supercov-rustc-spike-'));

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    env: {...process.env, ...options.env},
  });
  if (options.expectFailure) {
    assert.notEqual(result.status, 0, `${command} unexpectedly succeeded`);
  } else if (result.status !== 0) {
    process.stderr.write(result.stdout ?? '');
    process.stderr.write(result.stderr ?? '');
    throw new Error(`${command} exited ${result.status}`);
  }
  return result;
}

function records(directory) {
  return readdirSync(directory)
    .filter((name) => name.endsWith('.jsonl'))
    .flatMap((name) =>
      readFileSync(join(directory, name), 'utf8')
        .trim()
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line)),
    );
}

try {
  run('cargo', ['build', '--manifest-path', manifest], {
    env: {RUSTC_BOOTSTRAP: '1'},
  });

  const observedDirectory = join(scratch, 'observed');
  const observedTarget = join(scratch, 'observed-target');
  run('cargo', ['test', '--manifest-path', fixture, '--no-run'], {
    env: {
      CARGO_TARGET_DIR: observedTarget,
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUSTC_SPIKE_OUTPUT: observedDirectory,
    },
  });
  const observed = records(observedDirectory);
  const find = (definition) =>
    observed.find(
      (record) =>
        record.crate === 'supercov_rustc_spike_fixture' &&
        record.definition === definition,
    );

  const declarative = find('generated_by_rules');
  assert.equal(declarative?.expanded, true);
  assert.match(declarative.span, /src\/lib\.rs:9:/);
  assert.match(declarative.callsite, /src\/lib\.rs:15:/);

  const procedural = find('generated_by_proc');
  assert.equal(procedural?.expanded, true);
  assert.match(procedural.callsite, /src\/lib\.rs:16:/);

  const generated = find('generated_by_build_script');
  assert.equal(generated?.expanded, false);
  assert.match(generated.span, /\/out\/generated\.rs:/);

  assert.equal(find('const_decision')?.kind, 'Fn');
  assert.equal(find('CONST_VALUE')?.kind, 'Const');
  assert.equal(find('authored')?.expanded, false);

  const mutationDirectory = join(scratch, 'mutation');
  const mutation = run(
    'cargo',
    ['test', '--manifest-path', fixture, '--lib'],
    {
      expectFailure: true,
      env: {
        CARGO_TARGET_DIR: join(scratch, 'mutation-target'),
        RUSTC_WRAPPER: wrapper,
        SUPERCOV_RUSTC_SPIKE_OUTPUT: mutationDirectory,
        SUPERCOV_RUSTC_SPIKE_MUTATE_MIR: '1',
      },
    },
  );
  assert.match(`${mutation.stdout}\n${mutation.stderr}`, /left: 2[\s\S]*right: 1/);
  assert(records(mutationDirectory).some((record) => record.definition === 'authored'));

  const doctestDirectory = join(scratch, 'doctest');
  const doctest = run('cargo', ['test', '--manifest-path', fixture, '--doc'], {
    env: {
      CARGO_TARGET_DIR: join(scratch, 'doctest-target'),
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUSTC_SPIKE_OUTPUT: doctestDirectory,
    },
  });
  assert.match(doctest.stdout, /1 passed/);
  const doctestRecords = records(doctestDirectory);
  assert(
    !doctestRecords.some((record) => record.span.includes('src/lib.rs - (line 3)')),
    'ordinary RUSTC_WRAPPER unexpectedly observed rustdoc extracted source',
  );

  console.log(
    '[rustc-backend-spike] expanded HIR/MIR provenance and optimized-MIR replacement proved; ordinary RUSTC_WRAPPER does not observe the extracted doctest crate',
  );
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
