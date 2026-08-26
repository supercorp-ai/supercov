import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createHash, randomBytes} from 'node:crypto';
import {
  closeSync,
  cpSync,
  existsSync,
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
const supercov = join(root, 'target/debug/supercov');
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
  header.write('SCVRUST2', 0, 'ascii');
  header.writeUInt32LE(2, 8);
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
  assert.equal(bytes.subarray(0, 8).toString('ascii'), 'SCVRUST2');
  assert.equal(bytes.readUInt32LE(8), 2);
  assert.deepEqual(bytes.subarray(56, 72), transport.token);
  const descriptorCapacity = bytes.readUInt32LE(20);
  const payloadCapacity = bytes.readUInt32LE(24);
  const payloadBase = transportHeaderSize + descriptorCapacity * transportDescriptorSize;
  assert.equal(bytes.length, payloadBase + payloadCapacity);
  const reserved = Number(bytes.readBigUInt64LE(32));
  const ordinals = [];
  const decisions = [];
  const phases = [];
  let committed = 0;
  for (let index = 0; index < Math.min(reserved, descriptorCapacity); index += 1) {
    const descriptor = transportHeaderSize + index * transportDescriptorSize;
    if (bytes[descriptor] === 0) continue;
    committed += 1;
    const kind = bytes[descriptor + 1];
    const context = bytes.readBigUInt64LE(descriptor + 8).toString();
    const payloadOffset = bytes.readUInt32LE(descriptor + 16);
    const payloadLength = bytes.readUInt32LE(descriptor + 20);
    const idLength = bytes.readUInt32LE(descriptor + 24);
    const valueLength = bytes.readUInt32LE(descriptor + 28);
    assert(payloadOffset + payloadLength <= payloadCapacity);
    assert.equal(payloadLength, idLength + valueLength);
    const payload = payloadBase + payloadOffset;
    if (kind === 3) {
      assert.equal(payloadLength, 8);
      assert.equal(idLength, 0);
      assert.equal(valueLength, 8);
      ordinals.push({
        context,
        ordinal: bytes.readBigUInt64LE(payload).toString(),
      });
    } else if (kind === 2) {
      decisions.push({
        context,
        id: bytes.subarray(payload, payload + idLength).toString('utf8'),
        outcome: bytes[descriptor + 2] !== 0,
        values: [...bytes.subarray(payload + idLength, payload + payloadLength)].map(
          (value) => (value === 0 ? null : value === 2),
        ),
      });
    } else if (kind === 4) {
      assert.equal(bytes[descriptor + 2], 0);
      assert.equal(valueLength, 16);
      phases.push({
        child: context,
        parent: bytes.readBigUInt64LE(payload + idLength).toString(),
        nonce: bytes.readBigUInt64LE(payload + idLength + 8).toString(),
        decisionId: bytes.subarray(payload, payload + idLength).toString('utf8'),
      });
    } else {
      assert.fail(`unexpected Rust transport record kind ${kind}`);
    }
  }
  return {
    attachments: Number(bytes.readBigUInt64LE(72)),
    committed,
    dropped: Number(bytes.readBigUInt64LE(48)),
    incomplete: Math.min(reserved, descriptorCapacity) - committed,
    decisions,
    ordinals,
    phases,
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
    cwd: options.cwd ?? root,
    encoding: 'utf8',
    input: options.input,
    env: {
      ...process.env,
      SUPERCOV_RUST_SOURCE_ROOT: fixtureRoot,
      ...(commandEnvironment.CARGO_TARGET_DIR
        ? {SUPERCOV_RUST_TARGET_ROOT: commandEnvironment.CARGO_TARGET_DIR}
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

function compilerSources(directory, crate) {
  const snapshots = readdirSync(directory)
    .filter(
      (name) =>
        name.startsWith('sources-') &&
        name.endsWith(`-${crate}.json`),
    )
    .map((name) => JSON.parse(readFileSync(join(directory, name), 'utf8')));
  assert(snapshots.length > 0, `expected compiler source snapshots for ${crate}`);
  for (const snapshot of snapshots) {
    assert.equal(snapshot.schema, 'supercov-rust-source-snapshots-v1');
    assert.equal(snapshot.crate, crate);
    assert.deepEqual(
      snapshot.sources,
      snapshots[0].sources,
      `compiler source snapshots changed across ${crate} compilations`,
    );
  }
  return snapshots[0].sources;
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

function allManifestedHitOrdinals(directory) {
  return new Set(
    manifests(directory).flatMap((manifestRecord) => [
      ...manifestRecord.points.map(({probeOrdinal}) => probeOrdinal),
      ...manifestRecord.branches.flatMap(({alternatives}) =>
        alternatives.map(({probeOrdinal}) => probeOrdinal),
      ),
    ]),
  );
}

function obligationFor(manifestRecord, definition) {
  return manifestRecord.points.find(
    (obligation) =>
      obligation.kind === 'function' &&
      obligation.definitions.includes(definition),
  );
}

function obligationSource(sources, obligation) {
  const source = sources[obligation.sourceKey];
  assert(source, `missing source snapshot ${obligation.sourceKey}`);
  return source.source.slice(obligation.start, obligation.end);
}

function decisionFor(manifestRecord, definition) {
  return manifestRecord.decisions.find((decision) =>
    decision.definitions.includes(definition),
  );
}

function decisionForConditions(manifestRecord, definition, sources) {
  const matches = manifestRecord.decisions.filter(
    (decision) =>
      decision.definitions.includes(definition) &&
      JSON.stringify(decision.conditions.map(({source}) => source)) ===
        JSON.stringify(sources),
  );
  assert.equal(
    matches.length,
    1,
    `expected one ${definition} decision with conditions ${JSON.stringify(sources)}; found ${JSON.stringify(
      manifestRecord.decisions
        .filter((decision) => decision.definitions.includes(definition))
        .map((decision) => decision.conditions.map(({source}) => source)),
    )}`,
  );
  return matches[0];
}

function branchFor(manifestRecord, definition, kind) {
  const matches = branchesFor(manifestRecord, definition, kind);
  assert.equal(matches.length, 1, `expected one ${definition} ${kind} branch`);
  return matches[0];
}

function branchesFor(manifestRecord, definition, kind) {
  return manifestRecord.branches.filter(
    (branch) => branch.kind === kind && branch.definitions.includes(definition),
  ).sort((left, right) => left.start - right.start || left.end - right.end);
}

function matchGroupsFor(manifestRecord, definition) {
  return manifestRecord.selectionGroups
    .filter(
      (group) =>
        group.kind === 'match' && group.definitions.includes(definition),
    )
    .sort((left, right) => left.start - right.start || left.end - right.end);
}

function loopAlternativeOrdinals(branch) {
  const zero = branch.alternatives.find(
    ({label}) => label === 'zero iterations',
  )?.probeOrdinal;
  const entered = branch.alternatives.find(
    ({label}) => label === 'entered',
  )?.probeOrdinal;
  assert(zero && entered, `incomplete loop alternatives for ${branch.id}`);
  return {zero, entered};
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

function assertionPhaseContextId(parent, decisionId, nonce) {
  const digest = decisionId.replace(/^rs:decision:/, '');
  assert.match(digest, /^[0-9a-f]{24}$/);
  const bytes = Buffer.alloc(
    Buffer.byteLength('supercov-rust-assertion-phase-v2') + 8 + 8 + 4 + 8,
  );
  let offset = bytes.write('supercov-rust-assertion-phase-v2');
  bytes.writeBigUInt64LE(BigInt(parent), offset);
  offset += 8;
  bytes.writeBigUInt64LE(BigInt(`0x${digest.slice(0, 16)}`), offset);
  offset += 8;
  bytes.writeUInt32LE(Number.parseInt(digest.slice(16), 16), offset);
  offset += 4;
  bytes.writeBigUInt64LE(BigInt(nonce), offset);
  let value = 0xcbf29ce484222325n;
  for (const byte of bytes) {
    value ^= BigInt(byte);
    value = BigInt.asUintN(64, value * 0x100000001b3n);
  }
  if (value === 0n || value === 0xffffffffffffffffn) {
    value ^= 0xa5a55a5ad3c3b4b4n;
  }
  return value.toString();
}

function validatePhaseContexts(evidence, baseContexts) {
  const definitions = new Map();
  for (const phase of evidence.phases) {
    assert.equal(
      phase.child,
      assertionPhaseContextId(phase.parent, phase.decisionId, phase.nonce),
      'runtime phase definition failed deterministic authentication',
    );
    const serialized = `${phase.parent}:${phase.nonce}:${phase.decisionId}`;
    assert(
      !definitions.has(phase.child) || definitions.get(phase.child) === serialized,
      `phase context collision for ${phase.child}`,
    );
    definitions.set(phase.child, serialized);
  }
  const roots = new Set([...baseContexts].map(String));
  for (const start of [
    ...evidence.decisions.map(({context}) => context),
    ...evidence.ordinals.map(({context}) => context),
  ]) {
    if (start === '0' || roots.has(start)) continue;
    const path = new Set();
    let context = start;
    while (!roots.has(context)) {
      assert(!path.has(context), `phase context cycle at ${context}`);
      path.add(context);
      const definition = definitions.get(context);
      assert(definition, `phase context ${context} has no authenticated parent`);
      context = definition.slice(0, definition.indexOf(':'));
      assert.notEqual(context, '0', 'phase context crossed into background');
    }
  }
}

function assertionPhaseContexts(evidence, parent, decisionId) {
  return evidence.phases
    .filter(
      (phase) =>
        phase.parent === String(parent) && phase.decisionId === decisionId,
    )
    .map(({child}) => child);
}

function assertionPhaseContext(evidence, parent, decisionId) {
  const contexts = assertionPhaseContexts(evidence, parent, decisionId);
  assert.equal(
    contexts.length,
    1,
    `expected one dynamic phase for ${decisionId} under ${parent}`,
  );
  return contexts[0];
}

try {
  run('cargo', ['build', '--manifest-path', manifest], {
    env: {RUSTC_BOOTSTRAP: '1'},
  });
  run('cargo', ['build', '-p', 'supercov']);

  const rustc = run('rustup', ['which', 'rustc']).stdout.trim();
  const selectionRequest = {
    rustcPath: rustc,
    candidates: [wrapper],
    requirePublicCapabilities: false,
  };
  const selectedCompanion = JSON.parse(
    run(supercov, ['__select-rust-compiler-companion'], {
      input: JSON.stringify(selectionRequest),
    }).stdout,
  );
  assert.equal(selectedCompanion.rustcPath, rustc);
  assert.equal(selectedCompanion.companionPath, wrapper);
  assert.equal(selectedCompanion.handshake.protocolVersion, 1);
  assert.equal(selectedCompanion.handshake.frontendId, 'rust');
  assert.equal(
    selectedCompanion.handshake.coverageModelVariant,
    'rust-source-v1',
  );
  assert.equal(selectedCompanion.handshake.evidenceSchemaVersion, 3);
  assert.equal(
    selectedCompanion.handshake.companionBuildId,
    createHash('sha256').update(readFileSync(wrapper)).digest('hex'),
    'selector accepted a companion build ID that did not match its executable bytes',
  );
  assert.deepEqual(
    selectedCompanion.handshake.compiler,
    selectedCompanion.compiler,
    'selector accepted a companion built for a different compiler identity',
  );
  assert.equal(selectedCompanion.handshake.capabilities.ctfePathTracing, false);
  assert.equal(
    selectedCompanion.handshake.capabilities.rustdocDoctestTracing,
    false,
  );
  const publicSelection = run(
    supercov,
    ['__select-rust-compiler-companion'],
    {
      input: JSON.stringify({
        ...selectionRequest,
        requirePublicCapabilities: true,
      }),
      expectFailure: true,
    },
  );
  assert.match(
    publicSelection.stderr,
    /Rust companion lacks public coverage capabilities/,
  );
  const missingSelection = run(
    supercov,
    ['__select-rust-compiler-companion'],
    {
      input: JSON.stringify({...selectionRequest, candidates: []}),
      expectFailure: true,
    },
  );
  assert.match(
    missingSelection.stderr,
    /no exact compiler companion matches the selected rustc/,
  );
  const duplicateSelection = run(
    supercov,
    ['__select-rust-compiler-companion'],
    {
      input: JSON.stringify({...selectionRequest, candidates: [wrapper, wrapper]}),
      expectFailure: true,
    },
  );
  assert.match(
    duplicateSelection.stderr,
    /multiple exact compiler companions match the selected rustc/,
  );

  const productionFixture = join(scratch, 'production-fixture');
  cpSync(fixtureRoot, productionFixture, {
    recursive: true,
    filter: (path) =>
      !path.startsWith(join(fixtureRoot, 'target')) &&
      !path.startsWith(join(fixtureRoot, '.supercov')),
  });
  const productionRun = JSON.parse(
    run(supercov, ['__run-rust-compiler'], {
      env: {RUSTC: rustc},
      input: JSON.stringify({
        root: productionFixture,
        command: ['cargo', 'test'],
        runId: 'run_0123456789abcdef',
        startedAt: '2026-08-26T00:00:00.000Z',
        wrapperPath: supercov,
        companionCandidates: [wrapper],
        requirePublicCapabilities: false,
      }),
    }).stdout,
  );
  assert.equal(productionRun.exitCode, 0);
  assert.equal(productionRun.selection.companionPath, wrapper);
  assert(productionRun.denominator.points > 0);
  assert(productionRun.denominator.branches > 0);
  assert(productionRun.denominator.decisions > 0);
  assert(productionRun.artifacts > 0);
  assert(productionRun.tests > 0);
  assert.equal(productionRun.attemptHealth.length, productionRun.tests);
  assert(
    productionRun.attemptHealth.every(
      ({status, transport}) =>
        transport.dropped === 0 &&
        (status === 'skipped' || transport.attachments > 0),
    ),
    'production compiler run lost or dropped authenticated test evidence',
  );
  assert(productionRun.summary.lines.covered > 0);
  assert(productionRun.summary.branches.covered > 0);
  assert(productionRun.metadata.rawEvidence.files > productionRun.tests);
  assert(productionRun.metadata.rawEvidence.compressedBytes > 0);
  assert(
    productionRun.metadata.rawEvidence.compressedBytes <
      productionRun.metadata.rawEvidence.uncompressedBytes,
    'production compiler evidence archive was not compressed',
  );
  assert(
    existsSync(join(productionRun.runDirectory, 'evidence.raw.gz')),
    'production compiler run did not atomically publish its archive',
  );
  assert(
    !existsSync(
      join(
        productionFixture,
        '.supercov/work/run_0123456789abcdef',
      ),
    ),
    'production compiler run left terminal work state behind',
  );
  const productionQuery = run(
    supercov,
    ['runs', 'run_0123456789abcdef', '--json'],
    {cwd: productionFixture},
  );
  assert.match(productionQuery.stdout, /run_0123456789abcdef/);
  assert.equal(
    createHash('sha256')
      .update(readFileSync(join(productionFixture, 'src/lib.rs')))
      .digest('hex'),
    fixtureSourceDigest,
    'production compiler orchestration modified project source',
  );

  const observedDirectory = join(scratch, 'observed');
  const observedTarget = join(scratch, 'observed-target');
  run('cargo', ['test', '--manifest-path', fixture, '--no-run'], {
    env: {
      CARGO_TARGET_DIR: observedTarget,
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUST_COMPILER_OUTPUT: observedDirectory,
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
      SUPERCOV_RUST_COMPILER_OUTPUT: identityDirectoryA,
    },
  });
  run('cargo', ['build', '--quiet', '--manifest-path', fixture, '--lib'], {
    env: {
      CARGO_TARGET_DIR: join(scratch, 'identity-target-b'),
      RUSTC_WRAPPER: wrapper,
      SUPERCOV_RUST_COMPILER_OUTPUT: identityDirectoryB,
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
  const identityManifestPath = join(
    identityDirectoryA,
    readdirSync(identityDirectoryA).find(
      (name) =>
        name.startsWith('manifest-') &&
        name.endsWith('-supercov_rustc_spike_fixture.json'),
    ),
  );
  const productionValidation = run(
    supercov,
    ['__validate-rust-compiler-manifest'],
    {env: {SUPERCOV_INTERNAL_INPUT_FILE: identityManifestPath}},
  );
  assert.equal(productionValidation.stdout.trim(), 'supercov_rustc_spike_fixture');
  const normalized = JSON.parse(
    run(supercov, ['__normalize-rust-compiler-manifest'], {
      input: JSON.stringify({
        manifest: identityManifestA,
        sources: compilerSources(identityDirectoryA, 'supercov_rustc_spike_fixture'),
      }),
    }).stdout,
  );
  assert.equal(normalized.manifest.scope.language, 'rust');
  assert.equal(normalized.manifest.scope.crate, 'supercov_rustc_spike_fixture');
  assert.equal(normalized.manifest.points.length, identityManifestA.points.length);
  assert.equal(normalized.manifest.branches.length, identityManifestA.branches.length);
  assert.equal(normalized.manifest.decisions.length, identityManifestA.decisions.length);
  assert(
    Object.keys(normalized.hitObligationsByOrdinal).length >
      identityManifestA.points.length,
    'normalized ordinal resolver omitted branch alternatives',
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
    'RUST_MANIFEST_CANDIDATE_REMAINING_SURFACES: CTFE and doctest obligation/probe mappings are not emitted yet',
  ]);
  const allIds = [
    ...identityManifestA.points.map(({id}) => id),
    ...identityManifestA.branches.flatMap((branch) => [
      branch.id,
      ...branch.alternatives.map(({id}) => id),
    ]),
    ...identityManifestA.decisions.map(({id}) => id),
    ...identityManifestA.selectionGroups.map(({id}) => id),
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
  assert(
    identityManifestA.branches.every(({kind}) =>
      [
        'decision-outcome',
        'loop-entry',
        'match-arm',
        'let-else',
        'try-operator',
        'assertion-outcome',
      ].includes(kind),
    ),
    'compiler manifest emitted a branch kind outside the frozen Rust contract',
  );
  assert(
    identityManifestA.decisions.every(({kind}) =>
      [
        'if',
        'if-let',
        'while',
        'while-let',
        'let-chain',
        'match-guard',
        'assertion',
      ].includes(kind),
    ),
    'compiler manifest emitted a decision kind outside the frozen Rust contract',
  );
  for (const definition of [
    'for_values',
    'for_break',
    'two_for_values',
    'nested_for_values',
    'interrupted_for',
  ]) {
    assert(
      !identityManifestA.points.some(
        (point) =>
          point.definitions.includes(definition) &&
          point.provenance === 'synthetic-expansion',
      ),
      `compiler for-loop scaffolding leaked into ${definition} points`,
    );
  }
  const compoundDecision = decisionFor(identityManifestA, 'compound');
  assert.equal(compoundDecision?.kind, 'if');
  assert.equal(compoundDecision?.conditions.length, 2);
  assert.deepEqual(
    compoundDecision?.conditions.map(({source}) => source),
    ['left', 'right'],
  );
  assert.deepEqual(
    decisionFor(identityManifestA, 'disjoined')?.conditions.map(({source}) => source),
    ['left', 'right'],
  );
  assert.deepEqual(
    decisionFor(identityManifestA, 'mixed')?.conditions.map(({source}) => source),
    ['first', 'second', 'third'],
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
        SUPERCOV_RUST_COMPILER_OUTPUT: join(scratch, 'collision-output'),
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
        SUPERCOV_RUST_COMPILER_OUTPUT: join(scratch, 'probe-collision-output'),
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
    SUPERCOV_RUST_COMPILER_OUTPUT: instrumentedDirectory,
    SUPERCOV_RUST_INSTRUMENT_MIR: '1',
    SUPERCOV_RUST_TRANSPORT_FILE: behaviorTransport.path,
    SUPERCOV_RUST_TRANSPORT_TOKEN: behaviorTransport.tokenHex,
    SUPERCOV_RUST_CONTEXT_ID: transportContext.toString(16).padStart(16, '0'),
    LLVM_PROFILE_FILE: join(scratch, 'must-not-exist-%p.profraw'),
  };
  const instrumentedBehavior = run(
    'cargo',
    ['run', '--quiet', '--manifest-path', fixture, '--bin', 'behavior'],
    {env: instrumentedEnvironment},
  );
  assert.equal(instrumentedBehavior.stdout, baselineBehavior.stdout);
  assert.equal(instrumentedBehavior.stderr, baselineBehavior.stderr);
  assert.deepEqual(
    readdirSync(scratch).filter((name) => name.endsWith('.profraw')),
    [],
    'Supercov-owned instrumentation must not emit an LLVM profile',
  );
  const instrumentedBinary = readFileSync(
    join(scratch, 'instrumented-target/debug/behavior'),
  );
  assert(
    !instrumentedBinary.includes(Buffer.from('__llvm_profile')) &&
      !instrumentedBinary.includes(Buffer.from('__llvm_cov')),
    'the compiler companion linked native LLVM coverage machinery',
  );
  assert.match(baselineBehavior.stdout, /drop-order=\["panic-drop", "second", "first"\]/);
  assert.match(baselineBehavior.stdout, /decision-panic=true/);
  assert.match(baselineBehavior.stdout, /expanded=\[5, 3, 19, 17, 9\]/);
  assert.match(baselineBehavior.stdout, /conditions=\[29, 31, 31, 1, 37, 41, 43, 43\]/);
  assert.match(baselineBehavior.stdout, /or-mixed=\[47, 47, 49, 53, 59, 53, 59\]/);
  assert.match(baselineBehavior.stdout, /nested=\[79, 71, 73, 73\]/);
  assert.match(baselineBehavior.stdout, /nested-expression=\[89, 83, 89, 83, 89\]/);
  assert.match(baselineBehavior.stdout, /while=\[0, 2, 0\]/);
  assert.match(baselineBehavior.stdout, /while-let=\[0, 5, 0, 0\]/);
  assert.match(baselineBehavior.stdout, /for=\[0, 5\]/);
  assert.match(baselineBehavior.stdout, /for-break=\[0, 7\]/);
  assert.match(baselineBehavior.stdout, /for-two=\[2, 3\]/);
  assert.match(baselineBehavior.stdout, /for-nested=\[0, 5\]/);
  assert.match(baselineBehavior.stdout, /for-panic=true/);
  assert.match(baselineBehavior.stdout, /match-panic=true/);
  assert.match(
    baselineBehavior.stdout,
    /assertion-panics=\[true, true, false, false, true, false, true, true, true, false, false, true, false, true, true, true, false\]/,
  );
  assert.match(baselineBehavior.stdout, /assertion-edge-panics=\[true, true\]/);
  assert.match(baselineBehavior.stdout, /assertion-order=\["left", "right"\]/);
  assert.match(baselineBehavior.stdout, /match=\[3, 2, 2, 0\]/);
  assert.match(baselineBehavior.stdout, /match-identical=\[7, 7, 9\]/);
  assert.match(baselineBehavior.stdout, /match-empty=true/);
  assert.match(baselineBehavior.stdout, /match-irrefutable=5/);
  assert.match(baselineBehavior.stdout, /match-generated-nested-proc=\[13, 24, 0\]/);
  assert.match(
    baselineBehavior.stdout,
    /match-generated-nested-scrutinee-proc=\[1, 2, 0\]/,
  );
  assert.match(
    baselineBehavior.stdout,
    /match-generated-nested-guard-proc=\[3, 2, 2, 0\]/,
  );
  assert.match(baselineBehavior.stdout, /let-else=\[7, 0\]/);
  assert.match(baselineBehavior.stdout, /let-else-nested=\[7, 0, 0\]/);
  assert.match(baselineBehavior.stdout, /let-else-two=\[5, 0, 2\]/);
  assert.match(baselineBehavior.stdout, /let-else-generated-proc=\[8, 0\]/);
  assert.match(baselineBehavior.stdout, /let-else-generated-two-proc=\[5, 0, 2\]/);
  assert.match(baselineBehavior.stdout, /try-result=\[Ok\(8\), Err\("no"\)\]/);
  assert.match(baselineBehavior.stdout, /try-option=\[Some\(8\), None\]/);
  assert.match(
    baselineBehavior.stdout,
    /try-two=\[Ok\(5\), Err\("first"\), Err\("second"\)\]/,
  );
  assert.match(
    baselineBehavior.stdout,
    /try-generated-proc=\[Ok\(9\), Err\("no"\)\]/,
  );
  assert.match(
    baselineBehavior.stdout,
    /try-generated-two-proc=\[Ok\(5\), Err\("first"\), Err\("second"\)\]/,
  );
  assert.match(
    baselineBehavior.stdout,
    /try-nested=\[Ok\(8\), Err\("inner"\), Err\("outer"\)\]/,
  );
  assert.match(
    baselineBehavior.stdout,
    /try-generated-nested-proc=\[Ok\(8\), Err\("inner"\), Err\("outer"\)\]/,
  );
  assert.match(baselineBehavior.stdout, /try-panic=true/);
  assert.match(baselineBehavior.stdout, /match-unreachable=\[1, 2\]/);
  assert.match(baselineBehavior.stdout, /match-generated=\[23, 29\]/);
  assert.match(baselineBehavior.stdout, /match-generated-proc=\[31, 37\]/);
  assert.match(
    baselineBehavior.stdout,
    /match-generated-guarded-proc=\[3, 2, 2, 0\]/,
  );
  assert.match(baselineBehavior.stdout, /match-nested=\[3, 14, 0\]/);
  const runtimeManifest = crateManifest(
    instrumentedDirectory,
    'supercov_rustc_spike_fixture',
  );
  const runtimeSources = compilerSources(
    instrumentedDirectory,
    'supercov_rustc_spike_fixture',
  );
  const behaviorManifest = crateManifest(instrumentedDirectory, 'behavior');
  assert(
    behaviorManifest.decisions
      .filter((decision) => decision.definitions.includes('main'))
      .every((decision) => decision.kind === 'assertion'),
    'hidden assert!/println! implementation control flow leaked into the authored decision denominator',
  );
  const runtimeProbe = (definition) =>
    obligationFor(runtimeManifest, definition)?.probeOrdinal;
  const authoredProbe = runtimeProbe('authored');
  const fallibleProbe = runtimeProbe('fallible');
  const dropOrderProbe = runtimeProbe('drop_order');
  const panicProbe = runtimeProbe('panic_path');
  const whileInvocation = branchFor(
    runtimeManifest,
    'while_compound',
    'loop-entry',
  );
  const whileZero = whileInvocation.alternatives.find(
    ({label}) => label === 'zero iterations',
  )?.probeOrdinal;
  const whileEntered = whileInvocation.alternatives.find(
    ({label}) => label === 'entered',
  )?.probeOrdinal;
  const whileLetInvocation = branchFor(
    runtimeManifest,
    'while_let_chain',
    'loop-entry',
  );
  const whileLetZero = whileLetInvocation.alternatives.find(
    ({label}) => label === 'zero iterations',
  )?.probeOrdinal;
  const whileLetEntered = whileLetInvocation.alternatives.find(
    ({label}) => label === 'entered',
  )?.probeOrdinal;
  const {zero: forZero, entered: forEntered} = loopAlternativeOrdinals(
    branchFor(runtimeManifest, 'for_values', 'loop-entry'),
  );
  const {zero: forBreakZero, entered: forBreakEntered} =
    loopAlternativeOrdinals(
      branchFor(runtimeManifest, 'for_break', 'loop-entry'),
    );
  const twoForBranches = branchesFor(runtimeManifest, 'two_for_values', 'loop-entry');
  assert.equal(twoForBranches.length, 2, 'expected two sequential for branches');
  const twoForOrdinals = twoForBranches.map(loopAlternativeOrdinals);
  const nestedForBranches = branchesFor(
    runtimeManifest,
    'nested_for_values',
    'loop-entry',
  );
  assert.equal(nestedForBranches.length, 2, 'expected outer and inner for branches');
  const nestedForOrdinals = nestedForBranches.map(loopAlternativeOrdinals);
  const interruptedForOrdinals = loopAlternativeOrdinals(
    branchFor(runtimeManifest, 'interrupted_for', 'loop-entry'),
  );
  const letElseBranch = branchFor(runtimeManifest, 'let_else_value', 'let-else');
  const letElseMatched = letElseBranch.alternatives.find(
    ({label}) => label === 'matched',
  )?.probeOrdinal;
  const letElseFallback = letElseBranch.alternatives.find(
    ({label}) => label === 'else',
  )?.probeOrdinal;
  const letElseOrdinals = (branch) => ({
    matched: branch.alternatives.find(({label}) => label === 'matched')?.probeOrdinal,
    fallback: branch.alternatives.find(({label}) => label === 'else')?.probeOrdinal,
  });
  const nestedLetElse = letElseOrdinals(
    branchFor(runtimeManifest, 'nested_let_else', 'let-else'),
  );
  const twoLetElse = branchesFor(runtimeManifest, 'two_let_else', 'let-else')
    .sort((left, right) => left.start - right.start)
    .map(letElseOrdinals);
  assert.equal(twoLetElse.length, 2, 'expected two sequential let-else branches');
  const generatedLetElse = letElseOrdinals(
    branchFor(runtimeManifest, 'generated_let_else_by_proc', 'let-else'),
  );
  const generatedTwoLetElse = branchesFor(
    runtimeManifest,
    'generated_two_let_else_by_proc',
    'let-else',
  ).map(letElseOrdinals);
  assert.equal(
    generatedTwoLetElse.length,
    2,
    'expected two sequential synthetic let-else branches',
  );
  const additionalLetElseOrdinals = [
    nestedLetElse.matched,
    nestedLetElse.fallback,
    ...twoLetElse.flatMap(({matched, fallback}) => [matched, fallback]),
    generatedLetElse.matched,
    generatedLetElse.fallback,
    ...generatedTwoLetElse.flatMap(({matched, fallback}) => [matched, fallback]),
  ];
  const tryOrdinals = (branch) => ({
    continued: branch.alternatives.find(({label}) => label === 'continued')
      ?.probeOrdinal,
    returned: branch.alternatives.find(({label}) => label === 'early return')
      ?.probeOrdinal,
  });
  const tryResult = tryOrdinals(
    branchFor(runtimeManifest, 'try_result', 'try-operator'),
  );
  const tryOption = tryOrdinals(
    branchFor(runtimeManifest, 'try_option', 'try-operator'),
  );
  const twoTry = branchesFor(runtimeManifest, 'two_try_results', 'try-operator')
    .sort((left, right) => left.start - right.start)
    .map(tryOrdinals);
  assert.equal(twoTry.length, 2, 'expected two sequential authored try operators');
  const generatedTry = tryOrdinals(
    branchFor(runtimeManifest, 'generated_try_by_proc', 'try-operator'),
  );
  const generatedTwoTry = branchesFor(
    runtimeManifest,
    'generated_two_try_by_proc',
    'try-operator',
  ).map(tryOrdinals);
  assert.equal(
    generatedTwoTry.length,
    2,
    'expected two sequential synthetic try operators',
  );
  const panicTry = tryOrdinals(
    branchFor(runtimeManifest, 'panic_before_try', 'try-operator'),
  );
  const nestedTry = branchesFor(runtimeManifest, 'nested_try_result', 'try-operator')
    .sort((left, right) => left.start - right.start)
    .map(tryOrdinals);
  assert.equal(nestedTry.length, 2, 'expected two nested authored try operators');
  const generatedNestedTry = branchesFor(
    runtimeManifest,
    'generated_nested_try_by_proc',
    'try-operator',
  ).map(tryOrdinals);
  assert.equal(
    generatedNestedTry.length,
    2,
    'expected two nested synthetic try operators',
  );
  const committedTryOrdinals = [
    tryResult.continued,
    tryResult.returned,
    tryOption.continued,
    tryOption.returned,
    ...twoTry.flatMap(({continued, returned}) => [continued, returned]),
    generatedTry.continued,
    generatedTry.returned,
    ...generatedTwoTry.flatMap(({continued, returned}) => [continued, returned]),
    ...nestedTry.flatMap(({continued, returned}) => [continued, returned]),
    ...generatedNestedTry.flatMap(({continued, returned}) => [continued, returned]),
  ];
  const matchValueGroups = matchGroupsFor(runtimeManifest, 'match_value');
  const matchIdenticalGroups = matchGroupsFor(runtimeManifest, 'match_identical');
  const matchEmptyGroups = matchGroupsFor(runtimeManifest, 'match_empty');
  const generatedMatchGroups = matchGroupsFor(runtimeManifest, 'generated_match');
  const unreachableMatchGroups = matchGroupsFor(
    runtimeManifest,
    'match_unreachable',
  );
  const generatedProcMatchGroups = matchGroupsFor(
    runtimeManifest,
    'generated_match_by_proc',
  );
  const generatedGuardedProcMatchGroups = matchGroupsFor(
    runtimeManifest,
    'generated_guarded_match_by_proc',
  );
  const generatedNestedProcMatchGroups = matchGroupsFor(
    runtimeManifest,
    'generated_nested_match_by_proc',
  );
  const generatedNestedScrutineeProcMatchGroups = matchGroupsFor(
    runtimeManifest,
    'generated_nested_scrutinee_match_by_proc',
  );
  const generatedNestedGuardProcMatchGroups = matchGroupsFor(
    runtimeManifest,
    'generated_nested_guard_match_by_proc',
  );
  const nestedMatchGroups = matchGroupsFor(runtimeManifest, 'nested_match');
  const interruptedMatchGroups = matchGroupsFor(
    runtimeManifest,
    'interrupted_match',
  );
  assert.equal(matchValueGroups.length, 1);
  assert.equal(matchIdenticalGroups.length, 1);
  assert.equal(matchEmptyGroups.length, 1);
  assert.equal(generatedMatchGroups.length, 1);
  assert.equal(unreachableMatchGroups.length, 1);
  assert.equal(
    unreachableMatchGroups[0].arms.length,
    2,
    'a statically unreachable match arm remained in the branch denominator',
  );
  assert.equal(generatedProcMatchGroups.length, 1);
  assert.equal(generatedGuardedProcMatchGroups.length, 1);
  assert.equal(
    matchGroupsFor(runtimeManifest, 'match_irrefutable').length,
    0,
    'an irrefutable single-arm match created an impossible branch obligation',
  );
  assert.equal(nestedMatchGroups.length, 2);
  assert.equal(interruptedMatchGroups.length, 1);
  const matchSelectedOrdinals = [
    ...matchValueGroups,
    ...matchIdenticalGroups,
    ...matchEmptyGroups,
    ...generatedMatchGroups,
    ...unreachableMatchGroups,
    ...generatedProcMatchGroups,
    ...generatedGuardedProcMatchGroups,
    ...generatedNestedProcMatchGroups,
    ...generatedNestedScrutineeProcMatchGroups,
    ...generatedNestedGuardProcMatchGroups,
    ...nestedMatchGroups,
  ].flatMap((group) => group.arms.map(({selectedOrdinal}) => selectedOrdinal));
  const interruptedMatchOrdinals = interruptedMatchGroups[0].arms.flatMap(
    ({selectedOrdinal, notSelectedOrdinal}) => [
      selectedOrdinal,
      notSelectedOrdinal,
    ],
  );
  const committedForOrdinals = [
    forZero,
    forEntered,
    forBreakZero,
    forBreakEntered,
    ...twoForOrdinals.flatMap(({zero, entered}) => [zero, entered]),
    ...nestedForOrdinals.flatMap(({zero, entered}) => [zero, entered]),
  ];
  assert(
    [
      authoredProbe,
      fallibleProbe,
      dropOrderProbe,
      panicProbe,
      whileZero,
      whileEntered,
      whileLetZero,
      whileLetEntered,
      ...committedForOrdinals,
      letElseMatched,
      letElseFallback,
      ...additionalLetElseOrdinals,
      ...committedTryOrdinals,
      ...matchSelectedOrdinals,
    ].every(Boolean),
    'runtime probe is not bound to its manifest obligation',
  );
  const behaviorEvidence = readTransport(behaviorTransport);
  validatePhaseContexts(behaviorEvidence, [transportContext, 303, 404]);
  assert(
    behaviorEvidence.attachments > 0,
    'instrumented behavior did not attach any owned runtime',
  );
  assert.equal(behaviorEvidence.dropped, 0);
  assert.equal(
    behaviorEvidence.incomplete,
    3,
    'decision-condition, iterator-next and match-guard panics must remain explicit incomplete health',
  );
  const observedOrdinals = new Set(
    behaviorEvidence.ordinals.map(({ordinal}) => ordinal),
  );
  const previouslyProvenOrdinals = new Set([
      authoredProbe,
      fallibleProbe,
      dropOrderProbe,
      panicProbe,
      whileZero,
      whileEntered,
      whileLetZero,
      whileLetEntered,
      ...committedForOrdinals,
      letElseMatched,
      letElseFallback,
      ...additionalLetElseOrdinals,
      ...committedTryOrdinals,
      ...matchSelectedOrdinals,
  ]);
  assert(
    [...previouslyProvenOrdinals].every((ordinal) => observedOrdinals.has(ordinal)),
    'general point instrumentation lost a previously proven branch or function observation',
  );
  const manifestedHitOrdinals = allManifestedHitOrdinals(instrumentedDirectory);
  assert(
    [...observedOrdinals].every((ordinal) => manifestedHitOrdinals.has(ordinal)),
    'runtime emitted an ordinal outside the frozen point/branch denominator',
  );
  const fullyExecutedPointOrdinals = runtimeManifest.points
    .filter(({definitions}) =>
      definitions.some((definition) =>
        ['drop_order', 'panic_path', 'fallible'].includes(definition),
      ),
    )
    .map(({probeOrdinal}) => probeOrdinal);
  assert(fullyExecutedPointOrdinals.length > 4);
  assert(
    fullyExecutedPointOrdinals.every((ordinal) => observedOrdinals.has(ordinal)),
    'executed function/statement points did not all publish their manifest ordinals',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === letElseMatched).length,
    1,
    'the matched let-else alternative must commit exactly once',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === letElseFallback).length,
    1,
    'the else let-else alternative must commit exactly once',
  );
  for (const [ordinal, count] of [
    [nestedLetElse.matched, 1],
    [nestedLetElse.fallback, 2],
    [twoLetElse[0].matched, 2],
    [twoLetElse[0].fallback, 1],
    [twoLetElse[1].matched, 1],
    [twoLetElse[1].fallback, 1],
    [generatedLetElse.matched, 1],
    [generatedLetElse.fallback, 1],
  ]) {
    assert.equal(
      behaviorEvidence.ordinals.filter((hit) => hit.ordinal === ordinal).length,
      count,
      `let-else alternative ${ordinal} did not retain exact invocation count`,
    );
  }
  assert.deepEqual(
    generatedTwoLetElse
      .map(({matched}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === matched).length,
      )
      .sort(),
    [1, 2],
    'sequential synthetic let-else matched alternatives lost semantic order/counts',
  );
  assert.deepEqual(
    generatedTwoLetElse
      .map(({fallback}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === fallback).length,
      )
      .sort(),
    [1, 1],
    'sequential synthetic let-else fallback alternatives lost semantic order/counts',
  );
  for (const [ordinal, count] of [
    [tryResult.continued, 1],
    [tryResult.returned, 1],
    [tryOption.continued, 1],
    [tryOption.returned, 1],
    [twoTry[0].continued, 2],
    [twoTry[0].returned, 1],
    [twoTry[1].continued, 1],
    [twoTry[1].returned, 1],
    [generatedTry.continued, 1],
    [generatedTry.returned, 1],
    [nestedTry[0].continued, 2],
    [nestedTry[0].returned, 1],
    [nestedTry[1].continued, 1],
    [nestedTry[1].returned, 1],
  ]) {
    assert.equal(
      behaviorEvidence.ordinals.filter((hit) => hit.ordinal === ordinal).length,
      count,
      `try-operator alternative ${ordinal} did not retain exact invocation count`,
    );
  }
  assert.deepEqual(
    generatedTwoTry
      .map(({continued}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === continued).length,
      )
      .sort(),
    [1, 2],
    'sequential synthetic try continuations lost semantic order/counts',
  );
  assert.deepEqual(
    generatedTwoTry
      .map(({returned}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === returned).length,
      )
      .sort(),
    [1, 1],
    'sequential synthetic try residuals lost semantic order/counts',
  );
  assert.deepEqual(
    generatedNestedTry
      .map(({continued}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === continued).length,
      )
      .sort(),
    [1, 2],
    'nested synthetic try continuations lost semantic order/counts',
  );
  assert.deepEqual(
    generatedNestedTry
      .map(({returned}) =>
        behaviorEvidence.ordinals.filter((hit) => hit.ordinal === returned).length,
      )
      .sort(),
    [1, 1],
    'nested synthetic try residuals lost semantic order/counts',
  );
  assert.equal(
    behaviorEvidence.ordinals.some(({ordinal}) =>
      [panicTry.continued, panicTry.returned].includes(ordinal),
    ),
    false,
    'a panic while evaluating the try operand must not commit either alternative',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === whileZero).length,
    2,
    'two zero-iteration while invocations must remain distinct observations',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === whileEntered).length,
    1,
    'the entered while invocation must commit exactly once across all iterations',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === whileLetZero)
      .length,
    3,
    `three zero-iteration while-let invocations must remain distinct observations: ${JSON.stringify(
      behaviorEvidence.ordinals.filter(({ordinal}) =>
        [whileLetZero, whileLetEntered].includes(ordinal),
      ),
    )}`,
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === whileLetEntered)
      .length,
    1,
    'the entered while-let invocation must commit exactly once across iterations',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === forZero).length,
    1,
    'the empty for invocation must commit zero iterations exactly once',
  );
  assert.equal(
    behaviorEvidence.ordinals.filter(({ordinal}) => ordinal === forEntered).length,
    1,
    'the entered for invocation must commit exactly once across iterations',
  );
  for (const ordinal of committedForOrdinals.slice(2)) {
    assert.equal(
      behaviorEvidence.ordinals.filter((hit) => hit.ordinal === ordinal).length,
      1,
      `for alternative ${ordinal} must commit exactly once`,
    );
  }
  assert(
    !behaviorEvidence.ordinals.some(({ordinal}) =>
      [interruptedForOrdinals.zero, interruptedForOrdinals.entered].includes(ordinal),
    ),
    'a panicking iterator committed a false zero/entered alternative',
  );
  assert(
    !behaviorEvidence.ordinals.some(({ordinal}) =>
      interruptedMatchOrdinals.includes(ordinal),
    ),
    'a panicking match guard committed a false selected/not-selected alternative',
  );
  const allMatchGroups = runtimeManifest.selectionGroups.filter(
    ({kind}) => kind === 'match',
  );
  const allNotSelectedOrdinals = allMatchGroups.flatMap((group) =>
    group.arms.map(({notSelectedOrdinal}) => notSelectedOrdinal),
  );
  assert(
    !behaviorEvidence.ordinals.some(({ordinal}) =>
      allNotSelectedOrdinals.includes(ordinal),
    ),
    'derived match not-selected alternatives leaked into raw evidence',
  );
  const ordinalCount = (ordinal) =>
    behaviorEvidence.ordinals.filter((hit) => hit.ordinal === ordinal).length;
  assert.deepEqual(
    matchValueGroups[0].arms.map(({selectedOrdinal}) => ordinalCount(selectedOrdinal)),
    [1, 2, 1],
    'guarded match invocations did not select the exact authored arms',
  );
  assert.deepEqual(
    matchIdenticalGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1, 1],
    'identical match bodies lost distinct arm selection identity',
  );
  assert.deepEqual(
    matchEmptyGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1],
    'empty match bodies lost distinct arm selection identity',
  );
  assert.deepEqual(
    generatedMatchGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1],
    'a declarative-macro match lost authored arm selection identity',
  );
  assert.deepEqual(
    unreachableMatchGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1],
    'reachable arms changed when the compiler excluded an unreachable pattern',
  );
  assert.deepEqual(
    generatedProcMatchGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1],
    'a proc-macro match lost semantic arm marker identity after borrow checking',
  );
  assert.deepEqual(
    generatedGuardedProcMatchGroups[0].arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 2, 1],
    'a guarded proc-macro match lost semantic arm marker identity after borrow checking',
  );
  assert.equal(generatedNestedProcMatchGroups.length, 2);
  const generatedNestedProcRoot = generatedNestedProcMatchGroups.find(
    ({parentGroupId}) => parentGroupId === null,
  );
  const generatedNestedProcChild = generatedNestedProcMatchGroups.find(
    ({parentGroupId}) => parentGroupId === generatedNestedProcRoot?.id,
  );
  assert.deepEqual(
    generatedNestedProcRoot?.arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [2, 1],
    'the outer proc-macro match lost nested parent/arm identity',
  );
  assert.deepEqual(
    generatedNestedProcChild?.arms.map(({selectedOrdinal}) =>
      ordinalCount(selectedOrdinal),
    ),
    [1, 1],
    'the inner proc-macro match lost nested parent/arm identity',
  );
  const nestedSelectionCounts = (groups) => {
    const root = groups.find(({parentGroupId}) => parentGroupId === null);
    const child = groups.find(({parentGroupId}) => parentGroupId === root?.id);
    return {
      root: root?.arms.map(({selectedOrdinal}) => ordinalCount(selectedOrdinal)),
      child: child?.arms.map(({selectedOrdinal}) => ordinalCount(selectedOrdinal)),
      childSite: child?.parentSite,
    };
  };
  assert.deepEqual(
    nestedSelectionCounts(generatedNestedScrutineeProcMatchGroups),
    {root: [1, 1], child: [2, 1], childSite: 'scrutinee'},
    'a proc-macro match nested in a scrutinee lost semantic identity',
  );
  assert.deepEqual(
    nestedSelectionCounts(generatedNestedGuardProcMatchGroups),
    {root: [1, 2, 1], child: [2, 1], childSite: 'guard'},
    'a proc-macro match nested in a guard lost semantic identity',
  );
  assert.deepEqual(
    nestedMatchGroups.map((group) =>
      group.arms.map(({selectedOrdinal}) => ordinalCount(selectedOrdinal)),
    ),
    [
      [2, 1],
      [1, 1],
    ],
    'nested matches did not preserve independent outer and inner selections',
  );
  for (const group of allMatchGroups) {
    for (const selected of group.arms) {
      const rawSelections = ordinalCount(selected.selectedOrdinal);
      const derived = new Map(
        group.arms.map((arm) => [
          arm.branchId,
          {
            selected: arm.branchId === selected.branchId ? rawSelections : 0,
            notSelected: arm.branchId === selected.branchId ? 0 : rawSelections,
          },
        ]),
      );
      assert.equal(
        [...derived.values()].filter(({selected}) => selected > 0).length,
        rawSelections > 0 ? 1 : 0,
        `match selection ${group.id}/${selected.branchId} did not derive one selected arm`,
      );
      assert.equal(
        [...derived.values()].filter(({notSelected}) => notSelected > 0).length,
        rawSelections > 0 ? group.arms.length - 1 : 0,
        `match selection ${group.id}/${selected.branchId} did not derive every sibling rejection`,
      );
    }
  }
  const vectorsForDecision = (decision) => {
    const id = decision?.id;
    assert(id, 'missing runtime decision');
    return behaviorEvidence.decisions
      .filter((decision) => decision.id === id)
      .map(({values, outcome}) => JSON.stringify({values, outcome}))
      .sort();
  };
  const decisionVectors = (definition) =>
    vectorsForDecision(decisionFor(runtimeManifest, definition));
  assert(
    !behaviorEvidence.decisions.some(
      ({id}) => id === decisionFor(runtimeManifest, 'interrupted_decision')?.id,
    ),
    'an interrupted decision was incorrectly committed as a complete vector',
  );
  assert.deepEqual(decisionVectors('compound'), [
    JSON.stringify({values: [false, null], outcome: false}),
    JSON.stringify({values: [true, false], outcome: false}),
    JSON.stringify({values: [true, true], outcome: true}),
  ].sort());
  assert.deepEqual(decisionVectors('disjoined'), [
    JSON.stringify({values: [false, false], outcome: false}),
    JSON.stringify({values: [false, true], outcome: true}),
    JSON.stringify({values: [true, null], outcome: true}),
  ].sort());
  assert.deepEqual(decisionVectors('mixed'), [
    JSON.stringify({values: [false, false, null], outcome: false}),
    JSON.stringify({values: [false, true, true], outcome: true}),
    JSON.stringify({values: [true, null, false], outcome: false}),
    JSON.stringify({values: [true, null, true], outcome: true}),
  ].sort());
  assert.deepEqual(decisionVectors('pattern'), [
    JSON.stringify({values: [false], outcome: false}),
    JSON.stringify({values: [true], outcome: true}),
  ].sort());
  assert.deepEqual(decisionVectors('chained'), [
    JSON.stringify({values: [false, null, null], outcome: false}),
    JSON.stringify({values: [true, false, null], outcome: false}),
    JSON.stringify({values: [true, true, true], outcome: true}),
  ].sort());
  const assertionVectors = {
    compound: [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
    equality: [
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [true], outcome: true}),
    ].sort(),
  };
  assert.deepEqual(decisionVectors('assert_compound'), assertionVectors.compound);
  assert.deepEqual(decisionVectors('assert_equal'), assertionVectors.equality);
  assert.deepEqual(decisionVectors('assert_not_equal'), assertionVectors.equality);
  assert.deepEqual(
    decisionVectors('debug_assert_compound'),
    assertionVectors.compound,
  );
  assert.deepEqual(decisionVectors('debug_assert_equal'), assertionVectors.equality);
  assert.deepEqual(
    decisionVectors('debug_assert_not_equal'),
    assertionVectors.equality,
  );
  assert.deepEqual(
    decisionVectors('generated_assertion_by_proc'),
    assertionVectors.compound,
  );
  assert.deepEqual(decisionVectors('assert_panicking_message_argument'), [
    JSON.stringify({values: [false], outcome: false}),
  ]);
  assert.deepEqual(decisionVectors('assert_equal_evaluation_order'), [
    JSON.stringify({values: [true], outcome: true}),
  ]);
  assert(
    !behaviorEvidence.decisions.some(
      ({id}) => id === decisionFor(runtimeManifest, 'assert_panicking_condition')?.id,
    ),
    'an assertion condition panic was incorrectly committed as a failed assertion',
  );
  for (const definition of [
    'assert_compound',
    'assert_equal',
    'assert_not_equal',
    'debug_assert_compound',
    'debug_assert_equal',
    'debug_assert_not_equal',
    'generated_assertion_by_proc',
  ]) {
    const assertion = decisionFor(runtimeManifest, definition);
    assert.equal(assertion?.kind, 'assertion');
    const outcome = branchFor(runtimeManifest, definition, 'assertion-outcome');
    assert.deepEqual(
      outcome.alternatives.map(({label}) => label).sort(),
      ['failed', 'passed'],
    );
    assert(
      behaviorEvidence.decisions
        .filter(({id}) => id === assertion.id)
        .every(({context}) =>
          assertionPhaseContexts(
            behaviorEvidence,
            transportContext,
            assertion.id,
          ).includes(context),
        ),
      `${definition} evidence escaped its exact assertion phase`,
    );
  }
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'match_value', [
        'value > 0',
        'enabled',
      ]),
    ),
    [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    decisionVectors('generated_nested_guard_match_by_proc'),
    [
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [true], outcome: true}),
    ].sort(),
  );
  assert(
    !behaviorEvidence.decisions.some(
      ({id}) => id === decisionFor(runtimeManifest, 'interrupted_match')?.id,
    ),
    'a panicking match guard was incorrectly committed as a complete decision vector',
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'generated_guarded_match_by_proc', [
        'value > 0',
        'enabled',
      ]),
    ),
    [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(decisionVectors('generated_by_rules'), [
    JSON.stringify({values: [false], outcome: false}),
    JSON.stringify({values: [true], outcome: true}),
  ].sort());
  assert.deepEqual(decisionVectors('generated_by_proc'), [
    JSON.stringify({values: [false], outcome: false}),
  ]);
  assert.deepEqual(decisionVectors('repeated_expansions::generated_by_proc'), [
    JSON.stringify({values: [true], outcome: true}),
  ]);
  assert.deepEqual(decisionVectors('generated_by_build_script'), [
    JSON.stringify({values: [false], outcome: false}),
  ]);
  assert.deepEqual(
    vectorsForDecision(decisionForConditions(runtimeManifest, 'nested', ['first'])),
    [
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [true], outcome: true}),
      JSON.stringify({values: [true], outcome: true}),
      JSON.stringify({values: [true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'nested', ['second', 'third']),
    ),
    [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'nested_expression', [
        'first',
        '(if second { third } else { fourth })',
      ]),
    ),
    [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'nested_expression', ['second']),
    ),
    [
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [false], outcome: false}),
      JSON.stringify({values: [true], outcome: true}),
      JSON.stringify({values: [true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'while_compound', [
        'remaining > 0',
        'enabled',
      ]),
    ),
    [
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [false, null], outcome: false}),
      JSON.stringify({values: [true, false], outcome: false}),
      JSON.stringify({values: [true, true], outcome: true}),
      JSON.stringify({values: [true, true], outcome: true}),
    ].sort(),
  );
  assert.deepEqual(
    vectorsForDecision(
      decisionForConditions(runtimeManifest, 'while_let_chain', [
        'let Some(Some(value)) = values.pop()',
        'value > 0',
        'enabled',
      ]),
    ),
    [
      JSON.stringify({values: [false, null, null], outcome: false}),
      JSON.stringify({values: [false, null, null], outcome: false}),
      JSON.stringify({values: [true, false, null], outcome: false}),
      JSON.stringify({values: [true, true, false], outcome: false}),
      JSON.stringify({values: [true, true, true], outcome: true}),
      JSON.stringify({values: [true, true, true], outcome: true}),
    ].sort(),
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
        SUPERCOV_RUST_COMPILER_OUTPUT: ctfeDirectory,
        SUPERCOV_RUST_INSTRUMENT_CTFE: '1',
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
  assert(testEvidence.attachments > 0);
  assert.equal(testEvidence.dropped, 0);
  assert.equal(testEvidence.incomplete, 0);
  const testOrdinals = new Set(
    testEvidence.ordinals.map(({ordinal}) => ordinal),
  );
  assert(
    [authoredProbe, fallibleProbe, dropOrderProbe, panicProbe].every((ordinal) =>
      testOrdinals.has(ordinal),
    ),
    'the focused libtest lost its four original function observations',
  );
  assert(
    [...testOrdinals].every((ordinal) =>
      allManifestedHitOrdinals(instrumentedDirectory).has(ordinal),
    ),
    'the focused libtest emitted an ordinal outside its compiler manifests',
  );
  const pathPoints = runtimeManifest.points.filter(
    ({kind, definitions}) =>
      kind === 'statement' && definitions.includes('statement_paths'),
  );
  const pathOrdinal = (fragment) => {
    const matches = pathPoints.filter((point) =>
      obligationSource(runtimeSources, point).includes(`"${fragment}"`) &&
      !obligationSource(runtimeSources, point).includes('if value'),
    );
    assert.equal(matches.length, 1, `expected one statement point for ${fragment}`);
    return matches[0].probeOrdinal;
  };
  assert(testOrdinals.has(pathOrdinal('true-path')));
  assert(testOrdinals.has(pathOrdinal('after-path')));
  assert(
    !testOrdinals.has(pathOrdinal('false-path')),
    'unexecuted false-branch statement was falsely reported as covered',
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
  assert.match(concurrentTests.stdout, /9 passed/);
  const concurrentEvidence = readTransport(concurrentTransport);
  assert(concurrentEvidence.attachments > 0);
  assert.equal(concurrentEvidence.dropped, 0);
  assert.equal(concurrentEvidence.incomplete, 0);
  const contextNames = [
    'tests::context_one',
    'tests::context_two',
    'tests::attribute_context',
    'tests::panic_context',
    'tests::decision_context_true',
    'tests::decision_context_short_circuit',
  ];
  const contextIds = contextNames.map(testContextId);
  assert.equal(new Set(contextIds).size, contextIds.length, 'test context collision');
  const concurrentManifests = manifests(instrumentedDirectory);
  const assertionDecisionsFor = (name) => {
    const decisions = new Map(
      concurrentManifests.flatMap((manifestRecord) =>
        manifestRecord.decisions
          .filter(
            (decision) =>
              decision.kind === 'assertion' &&
              decision.definitions.includes(name),
          )
          .map((decision) => [decision.id, decision]),
      ),
    );
    return [...decisions.values()].sort(
      (left, right) =>
        right.end - right.start - (left.end - left.start) ||
        left.start - right.start,
    );
  };
  const assertionDecisionIdFor = (name) => {
    const decisions = assertionDecisionsFor(name);
    assert.equal(decisions.length, 1, `expected one assertion decision for ${name}`);
    return decisions[0].id;
  };
  const assertionContextIds = contextNames.map((name, index) => {
    return name === 'tests::panic_context'
      ? null
      : assertionPhaseContext(
          concurrentEvidence,
          contextIds[index],
          assertionDecisionIdFor(name),
        );
  });
  const resolvedAssertionContextIds = assertionContextIds.filter(Boolean);
  assert.equal(
    new Set([...contextIds, ...resolvedAssertionContextIds]).size,
    contextIds.length + resolvedAssertionContextIds.length,
    'test/assertion context collision',
  );
  const restoreTestContext = testContextId('tests::assertion_restore_context');
  const restoreAssertions = assertionDecisionsFor('tests::assertion_restore_context');
  assert.equal(restoreAssertions.length, 2);
  const restoreAuthoredAssertion = restoreAssertions.find((decision) =>
    decision.conditions.some(({source}) => source.includes('authored')),
  );
  assert(restoreAuthoredAssertion);
  const restoreAuthoredContext = assertionPhaseContext(
    concurrentEvidence,
    restoreTestContext,
    restoreAuthoredAssertion.id,
  );
  const nestedTestContext = testContextId('tests::nested_assertion_context');
  const nestedAssertions = assertionDecisionsFor('tests::nested_assertion_context');
  assert.equal(nestedAssertions.length, 2);
  const nestedOuterAssertion = nestedAssertions.find((decision) =>
    decision.conditions.some(({source}) => source.includes('fallible')),
  );
  const nestedInnerAssertion = nestedAssertions.find(
    (decision) => decision.id !== nestedOuterAssertion?.id,
  );
  assert(nestedOuterAssertion && nestedInnerAssertion);
  const nestedOuterContext = assertionPhaseContext(
    concurrentEvidence,
    nestedTestContext,
    nestedOuterAssertion.id,
  );
  const nestedInnerContext = assertionPhaseContext(
    concurrentEvidence,
    nestedOuterContext,
    nestedInnerAssertion.id,
  );
  validatePhaseContexts(concurrentEvidence, [
    ...contextIds,
    restoreTestContext,
    nestedTestContext,
    testContextId('tests::child_context'),
  ]);
  const concurrentOrdinalPairs = new Set(
    concurrentEvidence.ordinals.map(
      ({context, ordinal}) => `${context}:${ordinal}`,
    ),
  );
  const previouslyProvenContextPairs = new Set([
      `${assertionContextIds[0]}:${authoredProbe}`,
      `${assertionContextIds[1]}:${fallibleProbe}`,
      `${assertionContextIds[2]}:${authoredProbe}`,
      `${contextIds[3]}:${panicProbe}`,
      `${restoreAuthoredContext}:${authoredProbe}`,
      `${restoreTestContext}:${fallibleProbe}`,
      `${nestedInnerContext}:${authoredProbe}`,
      `${nestedOuterContext}:${fallibleProbe}`,
      `0:${authoredProbe}`,
  ]);
  assert(
    [...previouslyProvenContextPairs].every((pair) =>
      concurrentOrdinalPairs.has(pair),
    ),
    'general point instrumentation lost a previously proven exact-context observation',
  );
  assert(
    concurrentEvidence.ordinals.every(({ordinal}) =>
      allManifestedHitOrdinals(instrumentedDirectory).has(ordinal),
    ),
    'a concurrent statement/function hit lost its denominator identity',
  );
  const compoundDecisionId = decisionFor(runtimeManifest, 'compound')?.id;
  assert(compoundDecisionId);
  assert(
    concurrentEvidence.decisions.some(
      ({context, id, values, outcome}) =>
        context === assertionContextIds[4] &&
        id === compoundDecisionId &&
        JSON.stringify(values) === JSON.stringify([true, true]) &&
        outcome === true,
    ),
    'concurrent true decision vector lost its exact libtest context',
  );
  assert(
    concurrentEvidence.decisions.some(
      ({context, id, values, outcome}) =>
        context === assertionContextIds[5] &&
        id === compoundDecisionId &&
        JSON.stringify(values) === JSON.stringify([false, null]) &&
        outcome === false,
    ),
    'concurrent short-circuit vector lost its exact libtest context',
  );
  const isolatedTransport = createTransport('isolated-child-thread');
  const isolatedTestContext = testContextId('tests::child_context');
  const isolatedTest = run(
    'cargo',
    [
      'test',
      '--quiet',
      '--manifest-path',
      fixture,
      '--lib',
      'tests::child_context',
      '--',
      '--ignored',
      '--exact',
      '--test-threads=1',
    ],
    {
      env: {
        ...instrumentedEnvironment,
        SUPERCOV_RUST_TRANSPORT_FILE: isolatedTransport.path,
        SUPERCOV_RUST_TRANSPORT_TOKEN: isolatedTransport.tokenHex,
        SUPERCOV_RUST_CONTEXT_ID: BigInt(isolatedTestContext)
          .toString(16)
          .padStart(16, '0'),
      },
    },
  );
  assert.match(isolatedTest.stdout, /1 passed/);
  const isolatedEvidence = readTransport(isolatedTransport);
  validatePhaseContexts(isolatedEvidence, [isolatedTestContext]);
  assert(isolatedEvidence.attachments > 0);
  assert.equal(isolatedEvidence.dropped, 0);
  assert.equal(isolatedEvidence.incomplete, 0);
  assert(
    isolatedEvidence.ordinals.some(
      ({context, ordinal}) =>
        context === isolatedTestContext && ordinal === authoredProbe,
    ),
    'process-per-test fallback did not bind child-thread work to the exact test phase',
  );
  const isolatedAssertionId = assertionDecisionIdFor('tests::child_context');
  const isolatedAssertionContext = assertionPhaseContext(
    isolatedEvidence,
    isolatedTestContext,
    isolatedAssertionId,
  );
  assert(
    isolatedEvidence.decisions.some(
      ({context, id, outcome}) =>
        context === isolatedAssertionContext &&
        id === isolatedAssertionId &&
        outcome === true,
    ),
    'process-per-test fallback lost the parent assertion verdict',
  );
  const isolatedManifest = manifests(instrumentedDirectory).find(
    (manifestRecord) =>
      manifestRecord.crate === 'supercov_rustc_spike_fixture' &&
      manifestRecord.decisions.some(({id}) => id === isolatedAssertionId),
  );
  assert(isolatedManifest, 'isolated test compiler manifest was not emitted');
  const productionProjection = JSON.parse(
    run(supercov, ['__project-rust-compiler-evidence'], {
      input: JSON.stringify({
        normalization: {
          manifest: isolatedManifest,
          sources: compilerSources(
            instrumentedDirectory,
            'supercov_rustc_spike_fixture',
          ),
        },
        transportPath: isolatedTransport.path,
        tokenHex: isolatedTransport.tokenHex,
        baseContextId: isolatedTestContext,
        basePhase: {
          id: 'rust-test-phase',
          kind: 'test',
          operation: 'tests::child_context',
          source: 'src/lib.rs',
          causedByPhaseId: null,
          startedAtMs: 0,
          endedAtMs: 1,
          status: 'passed',
          error: null,
        },
      }),
    }).stdout,
  );
  assert.equal(productionProjection.health.dropped, 0);
  assert.equal(productionProjection.health.incomplete, 0);
  assert(productionProjection.attributed.hits.length > 0);
  assert.equal(productionProjection.background.hits.length, 0);
  assert(
    productionProjection.assertionPhases.some(
      ({status, causedByPhaseId}) =>
        status === 'passed' && causedByPhaseId === 'rust-test-phase',
    ),
    'production evidence projection lost the exact passed assertion phase',
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
      SUPERCOV_RUST_COMPILER_OUTPUT: doctestDirectory,
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
        SUPERCOV_RUST_COMPANION_PATH: wrapper,
        SUPERCOV_RUST_COMPILER_OUTPUT: wrappedDoctestDirectory,
        SUPERCOV_RUST_REAL_RUSTDOC: realRustdoc,
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
    '[rustc-backend-spike] expanded-HIR obligations keep deterministic identities; compiler mappings become exact Supercov nested/short-circuit/pattern/while/match-guard/assertion vectors and pre-optimization for-loop/match/let-else/try first-commit branches with libtest contexts, while MIR/CTFE/rustdoc interception preserves behavior and source',
  );
} finally {
  rmSync(scratch, {recursive: true, force: true});
}
