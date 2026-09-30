import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
const example = resolve(import.meta.dirname, '..');
const binary =
  process.env.SUPERCOV_BINARY ??
  resolve(example, '../../target/debug/supercov');
const root = mkdtempSync(resolve(tmpdir(), 'supercov-react-verification-'));
const recorded = resolve(example, 'recorded');
mkdirSync(recorded, { recursive: true });
for (const file of ['package.json', 'vitest.config.ts', 'src', 'tests'])
  cpSync(resolve(example, file), resolve(root, file), { recursive: true });
symlinkSync(
  resolve(example, 'node_modules'),
  resolve(root, 'node_modules'),
  'dir',
);
const original = readFileSync(resolve(root, 'src/Checkout.tsx'), 'utf8');
function execute(command, args, status = 0) {
  const r = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    timeout: 120000,
    maxBuffer: 20 * 1024 * 1024,
  });
  assert.ifError(r.error);
  assert.equal(r.status, status, r.stdout + r.stderr);
  return r.stdout;
}
function cli(args) {
  return execute(binary, args);
}
function query(args) {
  const r = JSON.parse(cli([...args, '--json']));
  assert.equal(r.ok, true);
  return r.data;
}
function save(name, value) {
  writeFileSync(
    resolve(recorded, name),
    typeof value === 'string' ? value : JSON.stringify(value, null, 2) + '\n',
  );
}
try {
  save('before.txt', cli(['--', 'npm', 'run', 'test:before']));
  const before = query(['runs', 'latest']);
  save('before.json', before);
  assert.equal(before.tests, 3);
  assert.equal(before.coverage.lines.percentage, 100);
  save('after.txt', cli(['--', 'npm', 'test']));
  const after = query(['runs', 'latest']);
  assert.equal(after.tests, 7);
  assert.equal(after.measurement.complete, true);
  // Assertion coverage for both suites, when a TypeSafe AI key is set: which of
  // the four UI expressions a test is judged to catch breaking. The deliberate
  // edits below check the same four expressions independently.
  const expressions = [
    '(quantity * 12.5).toFixed(2)',
    '`Pay for ${quantity} items`',
    'quantity === 0 || saving',
    "String('Payment failed. Try again.')",
  ];
  const assessments = {};
  if (process.env.TYPESAFE_API_KEY) {
    for (const [label, run] of [['before', before.run], ['after', after.run]]) {
      cli(['runs', run, 'assertions', 'assess']);
      const result = JSON.parse(
        readFileSync(resolve(root, '.supercov/runs', run, 'assertion-coverage.json'), 'utf8'),
      );
      assessments[label] = {
        percentage: result.summary.percentage,
        expressions: expressions.map((text) => ({
          text,
          asserted: result.statements.some(
            (s) => s.file === 'src/Checkout.tsx' && s.asserted && s.text.includes(text),
          ),
        })),
      };
    }
    save('ui-assertions.json', assessments);
  } else {
    console.log('TYPESAFE_API_KEY is not set: skipping the assertion coverage assessment.');
  }
  save('after.json', query(['runs', after.run]));
  // Deliberate edits test this example; Supercov does not perform mutation testing.
  const mutations = [
    ['wrong-total', 'quantity * 12.5', 'quantity * 125'],
    ['wrong-name', '`Pay for ${quantity} items`', '`Delete ${quantity} items`'],
    [
      'enabled-empty-order',
      'disabled={quantity === 0 || saving}',
      'disabled={false}',
    ],
    ['wrong-error', 'Payment failed. Try again.', 'Payment succeeded.'],
  ];
  const results = [];
  for (const [name, from, to] of mutations) {
    assert.ok(original.includes(from));
    writeFileSync(
      resolve(root, 'src/Checkout.tsx'),
      original.replace(from, to),
    );
    execute('npm', ['run', 'test:before']);
    save(`${name}.txt`, execute('npm', ['test'], 1));
    results.push({ name, weakSuitePassed: true, strongSuiteFailed: true });
  }
  save('result.json', {
    node: process.version,
    recordedAt: new Date().toISOString(),
    before: { tests: 3, lineCoverage: 100 },
    after: { tests: 7 },
    assessments,
    mutations: results,
  });
  console.log('3 weak tests: 100% lines. Four UI regressions still pass.');
  console.log('7 tests with focused assertions: all four regressions fail.');
  for (const [label, a] of Object.entries(assessments))
    console.log(
      `${label}: ${a.percentage}% asserted; UI expressions asserted: ${a.expressions.filter((e) => e.asserted).length} of 4.`,
    );
  console.log(`Evidence: ${recorded}`);
} finally {
  rmSync(root, { recursive: true, force: true });
}
