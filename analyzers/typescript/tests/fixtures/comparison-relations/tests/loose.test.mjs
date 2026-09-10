import { test } from 'node:test';
import * as assert from 'node:assert';
import { looseSelf, looseDeepSelf, explicitStrictSelf } from '../src/core.mjs';

test('Node loose self equality accepts NaN too', () => {
  const actual = looseSelf();
  assert.equal(actual, actual);
});
test('Node loose deep equality is reflexive', () => {
  const actual = looseDeepSelf();
  const expected = actual;
  assert.deepEqual(actual, expected);
});
test('explicit Node strict equality uses SameValue', () => {
  const actual = explicitStrictSelf();
  assert.strictEqual(actual, actual);
});
