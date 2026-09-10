import { test } from 'node:test';
import assert from 'node:assert/strict';
import { getLogger } from '../src/logger.mjs';

test('first logger count', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  const logger = getLogger();
  logger.info('hello', { a: 1 });
  logger.error('oops');
  // observes: src/logger.mjs#stdout console.log; check missing call
  assert.equal(log.mock.callCount(), 1);
  // observes: src/logger.mjs#stderr console.error; check missing call
  assert.equal(log.mock.callCount(), 1);
  // observes: src/logger.mjs#missing; check missing call
  assert.equal(error.mock.callCount(), 1);
});

test('later logger count is not a first-test proof', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = getLogger();
  logger.info('hello');
  // observes: src/logger.mjs#stdout console.log; check missing call
  assert.equal(log.mock.callCount(), 1);
});
