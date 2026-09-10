import { test } from 'node:test';
import assert, { ok as ensure } from 'node:assert/strict';
import renamed from 'node:assert';
import * as assertions from 'node:assert/strict';
import { strict as strictAssert } from 'node:assert';
import { okDiagnostic, callableDiagnostic, equalityDiagnostic, checkedValue,
  checkedTruthy, aliasTruthy, namedTruthy, namespaceTruthy, strictTruthy,
  shadowedValue, failedValue, mixedValue, throwingDiagnostic } from '../src/core.mjs';

test('ok ignores the returned diagnostic on success', () => {
  assert.ok(true, okDiagnostic());
});
test('callable assert ignores the returned diagnostic on success', () => {
  assert(true, callableDiagnostic());
});
test('equality ignores its third diagnostic argument on success', () => {
  assert.equal(1, 1, equalityDiagnostic());
});
test('positive control checks the return value', () => {
  assert.equal(checkedValue(), 7);
});
test('callable positive control checks truthiness', () => {
  // observes: src/core.mjs#checkedTruthy return true
  assert(checkedTruthy());
});
test('native import identities survive aliases', () => {
  renamed(aliasTruthy());
  ensure(namedTruthy());
  assertions.ok(namespaceTruthy());
  strictAssert(strictTruthy());
});
test('a local assert spelling is not a native identity', () => {
  const assert = (_value) => {};
  assert(shadowedValue());
});
test('caught failed assertion supplies no passing witness', () => {
  try { assert(failedValue()); } catch {}
});
test('mixed assertion outcomes do not supply a passing witness', () => {
  for (const value of [true, false]) {
    try { assert(mixedValue(value)); } catch {}
  }
});
test('diagnostic evaluation can still throw before the assertion', () => {
  assert.throws(() => assert.ok(true, throwingDiagnostic()), /diagnostic evaluation/);
});
