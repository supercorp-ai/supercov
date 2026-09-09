import { test } from 'node:test';
import assert from 'node:assert/strict';
import * as api from '../src/core.mjs';

test('call count', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.countOnly();
  assert.equal(log.mock.callCount(), 1);
});
test('history length', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.countLength();
  assert.equal(log.mock.calls.length, 1);
});
test('one argument', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.argumentsOnly();
  assert.equal(log.mock.calls[0].arguments[0], 'argument payload');
});
test('selected call', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.selectedCall();
  const calls = log.mock.calls;
  assert.deepEqual(calls[0].arguments, ['selected first']);
});
test('sliced call', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.slicedCall();
  const calls = log.mock.calls.slice(1);
  assert.deepEqual(calls[0].arguments, ['slice second']);
});
test('mapped arguments', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.mappedCalls();
  assert.deepEqual(log.mock.calls.map((call) => call.arguments[0]), ['mapped first', 'mapped second']);
});
test('conditional mocks', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  api.conditionalMocks();
  const chooseLog = process.env.CHOOSE_LOG !== 'no';
  const selected = chooseLog ? log : error;
  assert.equal(selected.mock.calls[0].arguments[0], chooseLog ? 'conditional left' : 'conditional right');
});
test('reset history', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.resetHistory();
  log.mock.resetCalls();
  assert.equal(log.mock.callCount(), 0);
  assert.deepEqual(log.mock.calls, []);
});
test('late mock', (t) => {
  api.lateMock();
  const log = t.mock.method(console, 'log', () => {});
  assert.equal(log.mock.callCount(), 0);
});
test('caught failure', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.failedWitness();
  try { assert.equal(log.mock.callCount(), 99); } catch {}
});
test('inactive assertion', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.inactiveWitness();
  if (false) assert.equal(log.mock.callCount(), 1);
});
test('same named tracker is not node:test', () => {
  const t = { mock: { method: () => ({ mock: { callCount: () => 1 } }) } };
  const log = t.mock.method(console, 'log', () => {});
  api.fakeTracker();
  assert.equal(log.mock.callCount(), 1);
});
test('shadowed console is not the production receiver', (t) => {
  const console = { log() {} };
  const log = t.mock.method(console, 'log', () => {});
  api.shadowedReceiver();
  assert.equal(log.mock.callCount(), 0);
});
