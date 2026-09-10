import test from 'node:test';
import assert from 'node:assert/strict';
import { throwsValue, noThrowValue, rejectsValue, noRejectValue,
  throwsMessage, noThrowMessage, rejectsMessage, noRejectMessage, expectedError } from '../src/core.mjs';

test('throws second string is a diagnostic with an ambiguity guard', () => {
  assert.throws(() => throwsValue(), throwsMessage());
});
test('doesNotThrow ignores normal callback result and second argument', () => {
  assert.doesNotThrow(() => noThrowValue(), noThrowMessage());
});
test('rejects second string is a diagnostic with an ambiguity guard', async () => {
  await assert.rejects(() => rejectsValue(), rejectsMessage());
});
test('doesNotReject ignores fulfillment value and second argument', async () => {
  await assert.doesNotReject(() => noRejectValue(), noRejectMessage());
});
test('throws second RegExp genuinely checks the exception text', () => {
  assert.throws(() => throwsValue(), expectedError());
});
