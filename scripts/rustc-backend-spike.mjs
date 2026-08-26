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
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const manifest = join(root, 'spikes/rustc-backend/Cargo.toml');
const fixture = join(root, 'spikes/rustc-backend/fixture/Cargo.toml');
const fixtureRoot = dirname(fixture);
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
    const context = bytes.readBigUInt64LE(descriptor + 8).toString();
    const payloadOffset = bytes.readUInt32LE(descriptor + 16);
    const payloadLength = bytes.readUInt32LE(descriptor + 20);
    const idLength = bytes.readUInt32LE(descriptor + 24);
    const valueLength = bytes.readUInt32LE(descriptor + 28);
    assert.equal(payloadLength, 8);
    assert.equal(idLength, 0);
    assert.equal(valueLength, 8);
    assert(payloadOffset + payloadLength <= payloadCapacity);
    ordinals.push({
      context,
      ordinal: bytes.readBigUInt64LE(payloadBase + payloadOffset).toString(),
    });
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
  const commandEnvironment = options.env ?? {};
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    env: {
      ...process.env,
      SUPERCOV_RUSTC_SPIKE_SOURCE_ROOT: fixtureRoot,
      ...(commandEnvironment.CARGO_TARGET_DIR
        ? {SUPERCOV_RUSTC_SPIKE_TARGET_ROOT: commandEnvironment.CARGO_TARGET_DIR}
        : {}),
      ...commandEnvironment,
    },
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

function manifests(directory) {
  return readdirSync(directory)
    .filter((name) => name.startsWith('manifest-') && name.endsWith('.json'))
    .map((name) => JSON.parse(readFileSync(join(directory, name), 'utf8')));
}

function crateManifest(directory, crate) {
  const matches = manifests(directory).filter(
    (manifestRecord) => manifestRecord.crate === crate,
  );
  assert.equal(
    matches.length,
    1,
    `expected one manifest candidate for ${crate}, found ${matches.length}`,
  );
  return matches[0];
}

function obligationFor(manifestRecord, definition) {
  return manifestRecord.points.find(
    (obligation) =>
      obligation.kind === 'function' &&
      obligation.definitions.includes(definition),
  );
}

function decisionFor(manifestRecord, definition) {
  return manifestRecord.decisions.find((decision) =>
    decision.definitions.includes(definition),
  );
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

function testContextId(testName) {
  let value = 0xcbf29ce484222325n;
  for (const byte of Buffer.from(`supercov-rust-test-v1\0${testName}`)) {
    value ^= BigInt(byte);
    value = BigInt.asUintN(64, value * 0x100000001b3n);
  }
  if (value === 0n || value === 0xffffffffffffffffn) {
    value ^= 0xa5a5a5a5a5a5a5a5n;
  }
  return value.toString();
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

  const identityDirectoryA = join(scratch, 'identity-a');
  const identityDirectoryB = join(scratch, 'identity-b');
  run('cargo', ['build', '--quiet', '--manifest-path', fixture, '--lib'], {
    env: {
      CARGO_TARGET_DIR: join(scratch, 'identity-target-a'),
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUSTC_SPIKE_OUTPUT: identityDirectoryA,
    },
  });
  run('cargo', ['build', '--quiet', '--manifest-path', fixture, '--lib'], {
    env: {
      CARGO_TARGET_DIR: join(scratch, 'identity-target-b'),
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUSTC_SPIKE_OUTPUT: identityDirectoryB,
    },
  });
  const identityManifestA = crateManifest(
    identityDirectoryA,
    'supercov_rustc_spike_fixture',
  );
  const identityManifestB = crateManifest(
    identityDirectoryB,
    'supercov_rustc_spike_fixture',
  );
  assert.deepEqual(
    identityManifestA,
    identityManifestB,
    'manifest candidate changed across clean target directories',
  );
  assert.equal(identityManifestA.schema, 'supercov-rust-manifest-candidate-v1');
  assert.equal(identityManifestA.model, 'rust-source-v1');
  assert.equal(identityManifestA.measurementComplete, false);
  assert.deepEqual(identityManifestA.limitations, [
    'RUST_MANIFEST_CANDIDATE_IF_SLICE_ONLY: loop, match, let-else, try, assertion, CTFE and doctest obligation/probe mappings are not emitted yet',
  ]);
  const allIds = [
    ...identityManifestA.points.map(({id}) => id),
    ...identityManifestA.branches.flatMap((branch) => [
      branch.id,
      ...branch.alternatives.map(({id}) => id),
    ]),
    ...identityManifestA.decisions.map(({id}) => id),
  ];
  assert.equal(
    new Set(allIds).size,
    allIds.length,
    'manifest candidate contains colliding obligation IDs',
  );
  assert(
    identityManifestA.points.some(({kind}) => kind === 'statement'),
    'compiler manifest did not emit statement points',
  );
  const compoundDecision = decisionFor(identityManifestA, 'compound');
  assert.equal(compoundDecision?.kind, 'if');
  assert.equal(compoundDecision?.conditions.length, 2);
  assert.deepEqual(
    compoundDecision?.conditions.map(({source}) => source),
    ['left', 'right'],
  );
  const patternDecision = decisionFor(identityManifestA, 'pattern');
  assert.equal(patternDecision?.kind, 'if-let');
  assert.equal(patternDecision?.conditions.length, 1);
  const chainedDecision = decisionFor(identityManifestA, 'chained');
  assert.equal(chainedDecision?.kind, 'let-chain');
  assert.deepEqual(
    chainedDecision?.conditions.map(({source}) => source),
    ['let Some(value) = value', 'value', 'enabled'],
  );
  const declarativeRoot = obligationFor(
    identityManifestA,
    'generated_by_rules',
  );
  const declarativeRepeated = obligationFor(
    identityManifestA,
    'repeated_expansions::generated_by_rules',
  );
  assert.equal(declarativeRoot?.id, declarativeRepeated?.id);
  assert.equal(declarativeRoot?.provenance, 'authored-expansion');
  assert.equal(declarativeRoot?.definitions.length, 2);
  const declarativeDecision = decisionFor(
    identityManifestA,
    'generated_by_rules',
  );
  assert.deepEqual(declarativeDecision?.definitions, [
    'generated_by_rules',
    'repeated_expansions::generated_by_rules',
  ]);
  const proceduralRoot = obligationFor(identityManifestA, 'generated_by_proc');
  const proceduralRepeated = obligationFor(
    identityManifestA,
    'repeated_expansions::generated_by_proc',
  );
  assert.equal(proceduralRoot?.provenance, 'synthetic-expansion');
  assert.equal(proceduralRepeated?.provenance, 'synthetic-expansion');
  assert.notEqual(proceduralRoot?.id, proceduralRepeated?.id);
  assert.notEqual(
    decisionFor(identityManifestA, 'generated_by_proc')?.id,
    decisionFor(identityManifestA, 'repeated_expansions::generated_by_proc')?.id,
  );
  assert.deepEqual(
    decisionFor(identityManifestA, 'generated_by_proc')?.conditions.map(
      ({source}) => source,
    ),
    ['value'],
  );
  const generatedObligation = obligationFor(
    identityManifestA,
    'generated_by_build_script',
  );
  assert.equal(generatedObligation?.provenance, 'generated-source');
  assert.equal(
    generatedObligation?.sourceKey,
    'generated:package:.:generated.rs',
  );
  assert(
    !JSON.stringify(identityManifestA).includes(scratch),
    'manifest candidate leaked an ephemeral target path',
  );
  const collision = run(
    'cargo',
    ['build', '--quiet', '--manifest-path', fixture, '--lib'],
    {
      expectFailure: true,
      env: {
        CARGO_TARGET_DIR: join(scratch, 'collision-target'),
        RUSTC_WRAPPER: wrapper,
        SUPERCOV_RUSTC_SPIKE_OUTPUT: join(scratch, 'collision-output'),
        SUPERCOV_RUSTC_SPIKE_FORCE_ID_COLLISION: '1',
      },
    },
  );
  assert.match(collision.stderr, /Supercov Rust obligation ID collision/);
  const probeCollision = run(
    'cargo',
    ['build', '--quiet', '--manifest-path', fixture, '--lib'],
    {
      expectFailure: true,
      env: {
        CARGO_TARGET_DIR: join(scratch, 'probe-collision-target'),
        RUSTC_WRAPPER: wrapper,
        SUPERCOV_RUSTC_SPIKE_OUTPUT: join(scratch, 'probe-collision-output'),
        SUPERCOV_RUSTC_SPIKE_FORCE_PROBE_COLLISION: '1',
      },
    },
  );
  assert.match(probeCollision.stderr, /Supercov Rust probe ordinal collision/);

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
  assert.match(baselineBehavior.stdout, /expanded=\[5, 3, 19, 17, 9\]/);
  assert.match(baselineBehavior.stdout, /conditions=\[29, 31, 37, 41, 43\]/);
  const runtimeManifest = crateManifest(
    instrumentedDirectory,
    'supercov_rustc_spike_fixture',
  );
  const runtimeProbe = (definition) =>
    obligationFor(runtimeManifest, definition)?.probeOrdinal;
  const authoredProbe = runtimeProbe('authored');
  const fallibleProbe = runtimeProbe('fallible');
  const dropOrderProbe = runtimeProbe('drop_order');
  const panicProbe = runtimeProbe('panic_path');
  assert(
    [authoredProbe, fallibleProbe, dropOrderProbe, panicProbe].every(Boolean),
    'runtime probe is not bound to a function manifest obligation',
  );
  const behaviorEvidence = readTransport(behaviorTransport);
  assert.equal(behaviorEvidence.attachments, 1);
  assert.equal(behaviorEvidence.dropped, 0);
  assert.equal(behaviorEvidence.incomplete, 0);
  assert.deepEqual(
    new Set(behaviorEvidence.ordinals.map(({ordinal}) => ordinal)),
    new Set([authoredProbe, fallibleProbe, dropOrderProbe, panicProbe]),
  );
  const behaviorPairs = new Set(
    behaviorEvidence.ordinals.map(({context, ordinal}) => `${context}:${ordinal}`),
  );
  assert(
    behaviorPairs.has(`303:${authoredProbe}`),
    'normal scope did not activate context 303',
  );
  assert(
    behaviorPairs.has(`404:${panicProbe}`),
    'panic scope did not activate context 404',
  );
  assert(
    behaviorPairs.has(`${transportContext}:${authoredProbe}`),
    `context was not restored after scope exit: ${JSON.stringify([...behaviorPairs])}`,
  );

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
  assert.deepEqual(
    new Set(testEvidence.ordinals.map(({ordinal}) => ordinal)),
    new Set([authoredProbe, fallibleProbe, dropOrderProbe, panicProbe]),
  );

  const concurrentTransport = createTransport('concurrent-tests');
  const concurrentTests = run(
    'cargo',
    [
      'test',
      '--quiet',
      '--manifest-path',
      fixture,
      '--lib',
      'context',
      '--',
      '--ignored',
      '--test-threads=5',
    ],
    {
      env: {
        ...instrumentedEnvironment,
        SUPERCOV_RUST_TRANSPORT_FILE: concurrentTransport.path,
        SUPERCOV_RUST_TRANSPORT_TOKEN: concurrentTransport.tokenHex,
        SUPERCOV_RUST_CONTEXT_ID: '0000000000000000',
      },
    },
  );
  assert.match(concurrentTests.stdout, /5 passed/);
  const concurrentEvidence = readTransport(concurrentTransport);
  assert.equal(concurrentEvidence.attachments, 1);
  assert.equal(concurrentEvidence.dropped, 0);
  assert.equal(concurrentEvidence.incomplete, 0);
  const contextNames = [
    'tests::context_one',
    'tests::context_two',
    'tests::attribute_context',
    'tests::panic_context',
  ];
  const contextIds = contextNames.map(testContextId);
  assert.equal(new Set(contextIds).size, contextIds.length, 'test context collision');
  assert.deepEqual(
    new Set(
      concurrentEvidence.ordinals.map(
        ({context, ordinal}) => `${context}:${ordinal}`,
      ),
    ),
    new Set([
      `${contextIds[0]}:${authoredProbe}`,
      `${contextIds[1]}:${fallibleProbe}`,
      `${contextIds[2]}:${authoredProbe}`,
      `${contextIds[3]}:${panicProbe}`,
      `0:${authoredProbe}`,
    ]),
  );
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
    '[rustc-backend-spike] compiler points, if/if-let/let-chain decisions and branches keep deterministic authored/macro/generated identities across clean targets; mmap function probes bind to manifest ordinals while MIR/CTFE/rustdoc interception preserves behavior and source',
  );
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
