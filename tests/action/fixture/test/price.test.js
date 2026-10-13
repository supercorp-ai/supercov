import assert from 'node:assert/strict';
import test from 'node:test';
import { discounted, total } from '../src/price.js';

test('total', () => {
  assert.equal(total(2, 3), 6);
});

test('no code', () => {
  assert.equal(discounted(10, ''), 10);
});
