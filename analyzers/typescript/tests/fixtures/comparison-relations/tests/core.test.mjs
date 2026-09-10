import { test } from 'node:test';
import assert from 'node:assert/strict';
import { selfOnly, aliasOnly, independent, overwritten, successive, withGetter, strictSelf, strictAlias, independentlyChecked, awaitedSelf, shadowed } from '../src/core.mjs';

test('self comparison', () => {
  const actual = selfOnly();
  // observes: src/core.mjs#selfOnly return
  assert.deepEqual(actual, actual);
});
test('immutable alias comparison', () => {
  const actual = aliasOnly();
  const expected = actual;
  const another = expected;
  assert.deepStrictEqual(actual, another);
});
test('independent comparison', () => {
  assert.deepEqual(independent(), 'fixed');
});
test('overwritten alias is independent', () => {
  const actual = overwritten();
  let expected = actual;
  expected = 'reset';
  assert.deepEqual(actual, expected);
});
test('repeated calls are separate evaluations', () => {
  assert.deepEqual(successive(), successive());
});
test('getters are separate evaluations', () => {
  const object = withGetter();
  assert.deepEqual(object.value, object.value);
});
test('Node strict self equality accepts NaN', () => {
  const actual = strictSelf();
  assert.equal(actual, actual);
});
test('an alias of a mutable binding can differ', () => {
  let actual = strictAlias();
  const expected = actual;
  actual = 23;
  assert.deepEqual(actual, expected);
});
test('independent evidence still counts', () => {
  const actual = independentlyChecked();
  assert.deepEqual(actual, actual);
  assert.deepEqual(actual, 'checked');
});
test('await is not an identity-preserving source wrapper', async () => {
  const actual = awaitedSelf();
  assert.deepEqual(await actual, actual);
});
test('a local helper named assert is not the native predicate', () => {
  const assert = {
    deepEqual(value, _expected) {
      if (value !== 'shadowed') throw new Error('unexpected value');
    },
  };
  const actual = shadowed();
  assert.deepEqual(actual, actual);
});
