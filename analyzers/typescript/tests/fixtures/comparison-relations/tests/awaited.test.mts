import test from 'node:test';
import assert from 'node:assert/strict';
import {
  awaitedReverse, awaitedAlias, awaitedTwice, awaitedIndependent, awaitedChecked,
  awaitedMutable, awaitedCalls, awaitedGetter, awaitedTyped,
} from '../src/core.mjs';

test('the expected operand may contain await', async () => {
  const actual = awaitedReverse();
  // observes: src/core.mjs#awaitedReverse return
  assert.deepEqual(actual, await actual);
});
test('awaited aliases retain input, not result, identity', async () => {
  const actual = awaitedAlias();
  const alias = actual;
  const resolved = await alias;
  const copied = resolved;
  assert.deepEqual(copied, actual);
});
test('two awaits are not generally a tautology', async () => {
  const actual = awaitedTwice();
  assert.deepEqual(await actual, await actual);
});
test('await with an independent expected value remains useful', async () => {
  const actual = awaitedIndependent();
  assert.deepEqual(await actual, 'independent awaited');
});
test('an independent assertion beside a dependent await remains useful', async () => {
  const actual = awaitedChecked();
  assert.deepEqual(await actual, actual);
  assert.deepEqual(await actual, 'checked awaited');
});
test('mutable bindings do not establish input identity across await', async () => {
  let actual = awaitedMutable();
  const expected = actual;
  actual = 'mutable awaited';
  assert.deepEqual(await actual, expected);
});
test('repeated awaited calls remain separate evaluations', async () => {
  assert.deepEqual(await awaitedCalls(), await awaitedCalls());
});
test('repeated awaited property reads remain separate evaluations', async () => {
  const object = awaitedGetter();
  assert.deepEqual(await object.value, object.value);
});
test('TypeScript-only syntax is transparent to awaited input tracing', async () => {
  const actual = awaitedTyped();
  const alias = (actual as string)!;
  assert.deepEqual(await (alias satisfies string), (actual as string));
});
