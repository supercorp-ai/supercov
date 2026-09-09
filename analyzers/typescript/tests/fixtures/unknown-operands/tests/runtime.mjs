import test from 'node:test';
import assert, { strictEqual } from 'node:assert/strict';
import { Assert } from 'node:assert';
import { capture } from './capture.mjs';
import { exact, smoke, opaque, inactive, failed, mixed, choose } from '../src/core.mjs';

// This basename must not be mistaken for Supercov's own runtime module.
test('awaited native assertion sources', async () => {
  assert.equal(await Promise.resolve(exact()), 4);
  strictEqual(await Promise.resolve(exact()), 4);
  const instance = new Assert();
  instance.strictEqual(await Promise.resolve(exact()), 4);
  assert.equal(exact(), 4);
});

test('opaque subprocess capture is not an absent assertion', async () => {
  const result = await capture();
  assert.equal(result.code, 3);
  assert.ok(result.stderr.includes('token:'));
});

test('opaque operand without synchronous statement attribution', async () => {
  const value = await Promise.resolve(String(opaque()));
  // The unsupported conversion warrants uncertainty, not assertion credit.
  assert.equal(Number.parseInt(value), 7);
});

test('unknown operands do not hide an untaken outcome', async () => {
  const value = await Promise.resolve(String(choose(true)));
  assert.equal(Number.parseInt(value), 10);
});

test('inactive unknown assertion is not a passing witness', async () => {
  const value = String(inactive());
  await Promise.resolve();
  if (false) assert.equal(Number.parseInt(value), 8);
});

test('caught failure is not a passing witness for an unknown operand', async () => {
  const value = String(failed());
  await Promise.resolve();
  try { assert.equal(Number.parseInt(value), 999); } catch {}
});

test('separate smoke test stays a test gap', () => {
  smoke();
});

test('mixed outcomes at one opaque assertion are not an all-passing witness', () => {
  const value = String(mixed());
  for (const expected of [12, 999]) {
    try { assert.equal(Number.parseInt(value), expected); } catch {}
  }
});
