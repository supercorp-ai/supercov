import test from 'node:test';
import assert from 'node:assert/strict';
import { syncNormal, asyncNormal, syncThrow, asyncThrow, throwFactory, normalFactory, fulfilledPromise, rejectedPromise } from '../src/core.mjs';

test('direct synchronous completion, not returned value', () => {
  // observes: src/core.mjs#syncNormal return 101;
  assert.doesNotThrow(syncNormal);
});
test('direct asynchronous completion, not fulfilled value', async () => {
  await assert.doesNotReject(asyncNormal);
});
test('direct synchronous exception', () => {
  assert.throws(syncThrow, /boom/);
});
test('direct asynchronous exception', async () => {
  await assert.rejects(asyncThrow, /rejected boom/);
});
test('factory evaluates before the exception assertion', () => {
  assert.throws(throwFactory(), /callback boom/);
});
test('factory returns the callback, not its result', () => {
  assert.doesNotThrow(normalFactory());
});
test('promise fulfillment completion, not fulfilled value', async () => {
  await assert.doesNotReject(fulfilledPromise());
});
test('promise rejection, not a synchronous producer exception', async () => {
  await assert.rejects(rejectedPromise(), /promise boom/);
});
