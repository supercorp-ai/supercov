import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nativeSharedLogger } from '../src/native-methods.mjs';
import * as arrays from '../src/arrays.mjs';
import * as api from '../src/core.mjs';
import { changed } from '../src/mutable.mjs';
import { createLogger, createTagged, sharedLogger, fromModuleClosure, sideEffectArgs,
  thisLogger, spreadLogger, inheritedLogger, conditionalClosure } from '../src/factories.mjs';
import { effectful } from '../src/effectful.mjs';
import { getterLogger } from '../src/getter.mjs';

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
test('object payload count', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.objectPayload();
  assert.equal(log.mock.callCount(), 1);
});
test('saved object payload count', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const saved = log;
  log.mock.restore();
  saved({ payload: [1, 2] });
  assert.equal(log.mock.callCount(), 1);
});
test('object argument side effects counted', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.nestedObjectPayload();
  assert.equal(log.mock.callCount(), 2);
});
test('closure payload is not invoked', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  api.parameter(() => console.error('must not be invoked'));
  assert.equal(log.mock.callCount(), 1);
  assert.equal(error.mock.callCount(), 0);
});
test('unmocked object payload limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  log.mock.restore();
  api.objectPayload();
  assert.equal(log.mock.callCount(), 0);
});
test('getter payload remains unsupported', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  console.log({ get value() { throw new Error('must not be inspected'); } });
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
test('discarded fresh object', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.opaque();
  assert.equal(log.mock.callCount(), 1);
});
test('fresh closure', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = createLogger({ enabled: true, stream: 'log' });
  logger.info('fresh payload');
  assert.equal(log.mock.callCount(), 1);
});
test('fresh quiet branch', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = createLogger({ enabled: false, stream: 'log' });
  logger.info('quiet payload');
  assert.equal(log.mock.callCount(), 0);
});
test('fresh receiver branch', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  const logger = createLogger({ enabled: true, stream: 'error' });
  logger.info('error payload');
  assert.equal(log.mock.callCount(), 0);
  assert.equal(error.mock.callCount(), 1);
});
test('separate closure environments', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const first = createTagged('first');
  const second = createTagged('second');
  first.info('one');
  second.info('two');
  assert.equal(log.mock.callCount(), 2);
});
test('pure module factory closure', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  fromModuleClosure();
  assert.equal(log.mock.callCount(), 1);
});
test('nested argument calls', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = sideEffectArgs();
  logger.info();
  assert.equal(log.mock.callCount(), 2);
});
test('fresh array spread', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = spreadLogger();
  logger.info(['first', 'second']);
  assert.equal(log.mock.callCount(), 1);
});
test('native factory method mutates shared receiver', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = nativeSharedLogger();
  logger.info('hello');
  assert.equal(log.mock.callCount(), 0);
});
test('array map counts callback effects', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.mappedCalls();
  assert.equal(log.mock.callCount(), 2);
});
test('array map routes callback effects', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  arrays.mappedRoutes();
  assert.equal(log.mock.callCount(), 1);
  assert.equal(error.mock.callCount(), 1);
});
test('array map output payload is not pinned', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.mappedOutput();
  assert.equal(log.mock.callCount(), 1);
});
test('array map evaluation order', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.mappedEvaluation();
  assert.equal(log.mock.callCount(), 4);
});
test('own map method is not array map', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const error = t.mock.method(console, 'error', () => {});
  arrays.ownMap();
  assert.equal(log.mock.callCount(), 0);
  assert.equal(error.mock.callCount(), 1);
});
test('sliced history survives reset', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  api.afterHistory();
  const tail = log.mock.calls.slice(1);
  log.mock.resetCalls();
  api.live();
  assert.equal(tail.length, 1);
  assert.equal(log.mock.callCount(), 1);
});
test('negative sliced history index', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  api.afterHistory();
  const tail = log.mock.calls.slice(-1);
  assert.equal(tail.length, 1);
});
test('nested and empty sliced history', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  api.afterHistory();
  api.live();
  const selected = log.mock.calls.slice(1).slice(0, 1);
  const empty = log.mock.calls.slice(9);
  assert.equal(selected.length, 1);
  assert.equal(empty.length, 0);
});
test('slice snapshots before argument effects', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.live();
  const selected = log.mock.calls.slice(arrays.sliceIndex());
  assert.equal(selected.length, 1);
  assert.equal(log.mock.callCount(), 2);
});
test('fresh array slice spreads selected values', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.slicedArray();
  assert.equal(log.mock.callCount(), 1);
});
test('sparse array map limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.sparseMap();
  assert.equal(log.mock.callCount(), 1);
});
test('mutating array map limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.mutatingMap();
  assert.equal(log.mock.callCount(), 1);
});
test('array map this argument limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  arrays.mapWithThis();
  assert.equal(log.mock.callCount(), 1);
});
test('coercing slice index limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  api.beforeHistory();
  api.afterHistory();
  const selected = log.mock.calls.slice('1');
  assert.equal(selected.length, 1);
});
test('shared module object limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = sharedLogger();
  logger.info();
  assert.equal(log.mock.callCount(), 0);
});
test('effectful module initialization limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  effectful();
  assert.equal(log.mock.callCount(), 1);
});
test('getter initialization limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = getterLogger();
  logger.info();
  assert.equal(log.mock.callCount(), 1);
});
test('receiver this limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = thisLogger();
  logger.info();
  assert.equal(log.mock.callCount(), 1);
});
test('missing own property limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = inheritedLogger({});
  logger.info('default payload');
  assert.equal(log.mock.callCount(), 1);
});
test('closure mutation limit', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const logger = createLogger({ enabled: true, stream: 'log' });
  logger.info = () => {};
  logger.info('ignored');
  assert.equal(log.mock.callCount(), 0);
});
test('captured branch environments', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  const enabled = conditionalClosure(true);
  const disabled = conditionalClosure(false);
  enabled();
  assert.equal(log.mock.callCount(), 1);
  log.mock.resetCalls();
  disabled();
  assert.equal(log.mock.callCount(), 0);
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
