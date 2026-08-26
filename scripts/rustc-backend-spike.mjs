import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createHash, randomBytes} from 'node:crypto';
import {
  closeSync,
  ftruncateSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readdirSync,
  rmSync,
  symlinkSync,
  writeSync,
} from 'node:fs';
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
const fixtureSourcePath = join(root, 'spikes/rustc-backend/fixture/src/lib.rs');
const fixtureSourceBytes = readFileSync(fixtureSourcePath);
const fixtureSourceDigest = createHash('sha256')
  .update(fixtureSourceBytes)
  .digest('hex');
const fixtureSource = fixtureSourceBytes.toString('utf8').split('\n');
const transportHeaderSize = 128;
const transportDescriptorSize = 40;
const transportContext = 42;

function createTransport(name, descriptorCapacity = 1024, payloadCapacity = 64 * 1024) {
  const path = join(scratch, `${name}.transport`);
  const token = randomBytes(16);
  const header = Buffer.alloc(transportHeaderSize);
  header.write('SCVRUST1', 0, 'ascii');
  header.writeUInt32LE(1, 8);
  header.writeUInt32LE(transportHeaderSize, 12);
  header.writeUInt32LE(transportDescriptorSize, 16);
  header.writeUInt32LE(descriptorCapacity, 20);
  header.writeUInt32LE(payloadCapacity, 24);
  header.writeUInt32LE(0x01020304, 28);
  token.copy(header, 56);
  const descriptorBytes = descriptorCapacity * transportDescriptorSize;
  const file = openSync(path, 'wx+');
  try {
    ftruncateSync(file, transportHeaderSize + descriptorBytes + payloadCapacity);
    writeSync(file, header, 0, header.length, 0);
  } finally {
    closeSync(file);
  }
  return {path, token, tokenHex: token.toString('hex')};
}

function readTransport(transport) {
  const bytes = readFileSync(transport.path);
  assert.equal(bytes.subarray(0, 8).toString('ascii'), 'SCVRUST1');
  assert.equal(bytes.readUInt32LE(8), 1);
  assert.deepEqual(bytes.subarray(56, 72), transport.token);
  const descriptorCapacity = bytes.readUInt32LE(20);
  const payloadCapacity = bytes.readUInt32LE(24);
  const payloadBase = transportHeaderSize + descriptorCapacity * transportDescriptorSize;
  assert.equal(bytes.length, payloadBase + payloadCapacity);
  const reserved = Number(bytes.readBigUInt64LE(32));
  const ordinals = [];
  let committed = 0;
  for (let index = 0; index < Math.min(reserved, descriptorCapacity); index += 1) {
    const descriptor = transportHeaderSize + index * transportDescriptorSize;
    if (bytes[descriptor] === 0) continue;
    committed += 1;
    const kind = bytes[descriptor + 1];
    if (kind !== 3) continue;
    assert.equal(Number(bytes.readBigUInt64LE(descriptor + 8)), transportContext);
    const payloadOffset = bytes.readUInt32LE(descriptor + 16);
    const payloadLength = bytes.readUInt32LE(descriptor + 20);
    const idLength = bytes.readUInt32LE(descriptor + 24);
    const valueLength = bytes.readUInt32LE(descriptor + 28);
    assert.equal(payloadLength, 8);
    assert.equal(idLength, 0);
    assert.equal(valueLength, 8);
    assert(payloadOffset + payloadLength <= payloadCapacity);
    ordinals.push(Number(bytes.readBigUInt64LE(payloadBase + payloadOffset)));
  }
  return {
    attachments: Number(bytes.readBigUInt64LE(72)),
    committed,
    dropped: Number(bytes.readBigUInt64LE(48)),
    incomplete: Math.min(reserved, descriptorCapacity) - committed,
    ordinals,
  };
}

function sourceLine(fragment) {
  const index = fixtureSource.findIndex((line) => line.includes(fragment));
  assert.notEqual(index, -1, `missing fixture fragment: ${fragment}`);
  return index + 1;
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    env: {...process.env, ...options.env},
  });
  if (result.status !== 0) {
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

function recordFiles(directory) {
  return readdirSync(directory)
    .filter((name) => name.endsWith('.jsonl'))
    .map((name) => ({
      name,
      records: readFileSync(join(directory, name), 'utf8')
        .trim()
        .split('\n')
        .filter(Boolean)
        .map((line) => JSON.parse(line)),
    }));
}

function normalizeTestOutput(output) {
  return output.replace(/\d+(?:\.\d+)?s/g, '<time>');
}

function passedTests(output) {
  return [...output.matchAll(/(\d+) passed/g)].reduce(
    (total, match) => total + Number(match[1]),
    0,
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
  assert.match(
    declarative.span,
    new RegExp(`src/lib\\.rs:${sourceLine('pub fn generated_by_rules')}:`),
  );
  assert.match(
    declarative.callsite,
    new RegExp(`src/lib\\.rs:${sourceLine('generated_function!();')}:`),
  );

  const procedural = find('generated_by_proc');
  assert.equal(procedural?.expanded, true);
  assert.match(
    procedural.callsite,
    new RegExp(
      `src/lib\\.rs:${sourceLine('probe_macros::generated_function!();')}:`,
    ),
  );

  const generated = find('generated_by_build_script');
  assert.equal(generated?.expanded, false);
  assert.match(generated.span, /\/out\/generated\.rs:/);

  assert.equal(find('const_decision')?.kind, 'Fn');
  assert.equal(find('CONST_VALUE')?.kind, 'Const');
  assert.equal(find('authored')?.expanded, false);

  const baselineBehavior = run(
    'cargo',
    ['run', '--quiet', '--manifest-path', fixture, '--bin', 'behavior'],
    {env: {CARGO_TARGET_DIR: join(scratch, 'baseline-behavior-target')}},
  );
  const instrumentedDirectory = join(scratch, 'instrumented');
  const behaviorTransport = createTransport('instrumented-behavior');
  const instrumentedEnvironment = {
    CARGO_TARGET_DIR: join(scratch, 'instrumented-target'),
    RUSTC_WRAPPER: wrapper,
    SUPERCOV_RUSTC_SPIKE_OUTPUT: instrumentedDirectory,
    SUPERCOV_RUSTC_SPIKE_INSTRUMENT_MIR: '1',
    SUPERCOV_RUST_TRANSPORT_FILE: behaviorTransport.path,
    SUPERCOV_RUST_TRANSPORT_TOKEN: behaviorTransport.tokenHex,
    SUPERCOV_RUST_CONTEXT_ID: transportContext.toString(16).padStart(16, '0'),
  };
  const instrumentedBehavior = run(
    'cargo',
    ['run', '--quiet', '--manifest-path', fixture, '--bin', 'behavior'],
    {env: instrumentedEnvironment},
  );
  assert.equal(instrumentedBehavior.stdout, baselineBehavior.stdout);
  assert.equal(instrumentedBehavior.stderr, baselineBehavior.stderr);
  assert.match(baselineBehavior.stdout, /drop-order=\["panic-drop", "second", "first"\]/);
  const behaviorEvidence = readTransport(behaviorTransport);
  assert.equal(behaviorEvidence.attachments, 1);
  assert.equal(behaviorEvidence.dropped, 0);
  assert.equal(behaviorEvidence.incomplete, 0);
  assert.deepEqual(new Set(behaviorEvidence.ordinals), new Set([0, 1, 2, 3]));

  const ctfeDirectory = join(scratch, 'ctfe');
  const ctfeBehavior = run(
    'cargo',
    ['run', '--quiet', '--manifest-path', fixture, '--bin', 'behavior'],
    {
      env: {
        CARGO_TARGET_DIR: join(scratch, 'ctfe-target'),
        RUSTC_WRAPPER: wrapper,
        SUPERCOV_RUSTC_SPIKE_OUTPUT: ctfeDirectory,
        SUPERCOV_RUSTC_SPIKE_INSTRUMENT_CTFE: '1',
      },
    },
  );
  assert.equal(ctfeBehavior.stdout, baselineBehavior.stdout);
  assert.equal(ctfeBehavior.stderr, baselineBehavior.stderr);
  assert.match(ctfeBehavior.stdout, /const-values=11,13/);
  const ctfeSequences = recordFiles(ctfeDirectory)
    .filter(({name}) => name.endsWith('-ctfe.jsonl'))
    .map(({records: fileRecords}) =>
      fileRecords.map((record) => `${record.observationKind}:${record.ordinal}`),
    );
  assert(
    ctfeSequences.some((observations) =>
      [
        'block:0',
        'block:1',
        'block:2',
        'block:3',
        'edge:0',
        'edge:1',
      ].every((observation) => observations.includes(observation)),
    ),
    `expected both concurrency-safe CTFE edges and all original blocks, got ${JSON.stringify(ctfeSequences)}`,
  );

  const testTransport = createTransport('instrumented-test');
  const probeTest = run(
    'cargo',
    [
      'test',
      '--quiet',
      '--manifest-path',
      fixture,
      '--lib',
      'records_real_runtime_probes',
      '--',
      '--ignored',
    ],
    {
      env: {
        ...instrumentedEnvironment,
        SUPERCOV_RUST_TRANSPORT_FILE: testTransport.path,
        SUPERCOV_RUST_TRANSPORT_TOKEN: testTransport.tokenHex,
      },
    },
  );
  assert.match(probeTest.stdout, /1 passed/);
  const testEvidence = readTransport(testTransport);
  assert.equal(testEvidence.attachments, 1);
  assert.equal(testEvidence.dropped, 0);
  assert.equal(testEvidence.incomplete, 0);
  assert.deepEqual(new Set(testEvidence.ordinals), new Set([0, 1, 2, 3]));
  const instrumentedRecords = records(instrumentedDirectory);
  assert(instrumentedRecords.some((record) => record.definition === 'authored'));
  const injectedProbe = instrumentedRecords.find((record) =>
    record.definition.endsWith('__supercov_spike_runtime::ordinal_hit'),
  );
  assert.match(injectedProbe?.span ?? '', /<supercov-rust-runtime>/);
  assert.equal(
    createHash('sha256').update(readFileSync(fixtureSourcePath)).digest('hex'),
    fixtureSourceDigest,
    'the compiler companion modified the fixture source',
  );

  const doctestDirectory = join(scratch, 'doctest');
  const doctest = run('cargo', ['test', '--quiet', '--manifest-path', fixture, '--doc'], {
    env: {
      CARGO_TARGET_DIR: join(scratch, 'doctest-target'),
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUSTC_SPIKE_OUTPUT: doctestDirectory,
    },
  });
  assert.equal(passedTests(doctest.stdout), 3);
  const doctestRecords = records(doctestDirectory);
  assert(
    !doctestRecords.some((record) => record.span.includes('src/lib.rs - (line 3)')),
    'ordinary RUSTC_WRAPPER unexpectedly observed rustdoc extracted source',
  );

  const rustdocLauncher = join(scratch, 'supercov-rustdoc-backend-spike');
  symlinkSync(wrapper, rustdocLauncher);
  const realRustdoc = run('rustup', ['which', 'rustdoc']).stdout.trim();
  const wrappedDoctestDirectory = join(scratch, 'wrapped-doctest');
  const wrappedDoctest = run(
    'cargo',
    ['test', '--quiet', '--manifest-path', fixture, '--doc'],
    {
      env: {
        CARGO_TARGET_DIR: join(scratch, 'wrapped-doctest-target'),
        RUSTDOC: rustdocLauncher,
        SUPERCOV_RUSTC_SPIKE_COMPANION_PATH: wrapper,
        SUPERCOV_RUSTC_SPIKE_OUTPUT: wrappedDoctestDirectory,
        SUPERCOV_RUSTC_SPIKE_REAL_RUSTDOC: realRustdoc,
      },
    },
  );
  assert.equal(
    normalizeTestOutput(wrappedDoctest.stdout),
    normalizeTestOutput(doctest.stdout),
  );
  assert.equal(
    normalizeTestOutput(wrappedDoctest.stderr),
    normalizeTestOutput(doctest.stderr),
  );
  const wrappedDoctestRecords = records(wrappedDoctestDirectory);
  const standalone = wrappedDoctestRecords.find(
    (record) => record.doctestRole === 'standalone',
  );
  assert.match(standalone?.doctestPath ?? '', /(^|\/)src\/lib\.rs$/);
  assert.match(standalone?.doctestLine ?? '', /^\d+$/);
  const standaloneAuthoredLines = wrappedDoctestRecords
    .filter((record) => record.doctestRole === 'standalone')
    .flatMap((record) => record.mirAuthoredLines ?? []);
  assert(
    standaloneAuthoredLines.includes(sourceLine('# let hidden')),
    `hidden doctest line was not mapped to authored source: ${JSON.stringify(standaloneAuthoredLines)}`,
  );
  assert(
    standaloneAuthoredLines.includes(sourceLine('assert_eq!(hidden + 2')),
    `visible doctest line was not mapped to authored source: ${JSON.stringify(standaloneAuthoredLines)}`,
  );
  assert(
    wrappedDoctestRecords.some(
      (record) =>
        record.doctestRole === 'merged-bundle' &&
        record.definition.includes('__doctest_0'),
    ),
    'the companion did not observe merged doctest user code',
  );
  assert(
    wrappedDoctestRecords.some(
      (record) =>
        record.doctestRole === 'merged-runner' &&
        record.definition.includes('__doctest_0'),
    ),
    'the companion did not observe the merged doctest identity map',
  );
  const mergedIdentity = wrappedDoctestRecords.find(
    (record) =>
      record.doctestRole === 'merged-runner' &&
      record.definition === '__doctest_0::TEST',
  );
  assert.match(mergedIdentity?.bodySnippet ?? '', /src\/lib\.rs/);
  assert.match(mergedIdentity?.bodySnippet ?? '', /line 3/);
  assert.equal(
    createHash('sha256').update(readFileSync(fixtureSourcePath)).digest('hex'),
    fixtureSourceDigest,
    'the rustdoc companion modified the fixture source',
  );

  console.log(
    '[rustc-backend-spike] expanded provenance, mmap runtime MIR probes and CTFE markers preserve behavior; a scoped rustdoc test-builder companion observes standalone and merged doctest identities without output or checkout changes',
  );
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
