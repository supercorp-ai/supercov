import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as api from '../src/core.mjs';
import { changed } from '../src/mutable.mjs';

test('live', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.live();
  // observes: src/core.mjs#live console.log
  assert.equal(log.mock.callCount(), 1);
});
test('reset', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeReset();
  log.mock.resetCalls();
  api.afterReset();
  assert.equal(log.mock.calls.length, 1);
});
test('wrong receiver', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  api.wrongReceiver();
  assert.equal(log.mock.callCount(), 0);
});
test('replacement', (t) => {
  const first = t.mock.method(console, 'log', () => {});
  const second = t.mock.method(console, 'log', () => {});
  api.replaced();
  assert.equal(first.mock.callCount(), 0);
  assert.equal(second.mock.callCount(), 1);
});
test('restore', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  log.mock.restore();
  api.restored();
  assert.equal(log.mock.callCount(), 0);
});
test('restore previous', (t) => {
  const first = t.mock.method(console, 'log', () => {});
  const second = t.mock.method(console, 'log', () => {});
  second.mock.restore();
  api.previous();
  assert.equal(first.mock.callCount(), 1);
  assert.equal(second.mock.callCount(), 0);
});
test('saved spy', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const saved = log;
  log.mock.restore();
  saved('direct test call');
  assert.equal(log.mock.callCount(), 1);
});
test('count snapshot', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeSnapshot();
  const count = log.mock.callCount();
  api.afterSnapshot();
  assert.equal(count, 1);
});
test('history snapshot', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  const history = log.mock.calls;
  log.mock.resetCalls();
  api.afterHistory();
  assert.equal(history.length, 1);
  assert.equal(log.mock.callCount(), 1);
});
test('repeated occurrence', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.twice();
  api.twice();
  assert.equal(log.mock.callCount(), 2);
});
test('parameter', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const value = 'parameter payload';
  api.parameter(value);
  assert.equal(log.mock.callCount(), 1);
});
test('conditional limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  if (true) api.conditional();
  assert.equal(log.mock.callCount(), 1);
});
test('async limit', async (t) => {
  const log = t.mock.method(console, 'log', () => {});
  await Promise.resolve();
  api.asynchronous();
  assert.equal(log.mock.callCount(), 1);
});
test('escape limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.escape(log);
  api.escaped();
  assert.equal(log.mock.callCount(), 1);
});
test('self count limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.selfCount();
  const count = log.mock.callCount();
  assert.equal(count, count);
});
test('opaque producer limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.opaque();
  assert.equal(log.mock.callCount(), 1);
});
test('reassigned function limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  changed();
  assert.equal(log.mock.callCount(), 1);
});
test('replacement callback limit', (t) => {
  const log = t.mock.method(console, 'log', () => 42);
  api.live();
  assert.equal(log.mock.callCount(), 1);
});
test('mutated snapshot limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  const history = log.mock.calls;
  history.shift();
  assert.equal(history.length, 0);
});
