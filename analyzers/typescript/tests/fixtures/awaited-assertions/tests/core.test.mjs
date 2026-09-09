import test from 'node:test';
import assert, { strictEqual as equal } from 'node:assert/strict';
import { value } from '../src/value.mjs';

const events = [];
function operand(label, result = value()) {
  // Argument evaluation must remain outside the assertion invocation phase.
  if (globalThis.__SUPERCOV_DIRECT_RUNTIME__?.coverageCarrier().phaseId)
    throw new Error('premature assertion phase');
  events.push(label);
  return Promise.resolve(result);
}
function trace(name) {
  console.log('TRACE ' + JSON.stringify([name, events.splice(0)]));
}

test('awaited values preserve order and synchronous return values', async () => {
  queueMicrotask(() => events.push('microtask'));
  const result = assert.equal(await operand('actual'), await operand('expected')); // witness:passed
  if (result !== undefined) throw new Error('changed return value');
  equal(await operand('alias'), 4); // witness:passed
  assert(await operand('callable', true)); // witness:passed
  assert['equal'](...await operand('spread', [4, 4])); // witness:passed
  trace('values');
});

test('interleaved assertions retain independent call sites', async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const first = (async () => {
    assert.equal(await gate, 4); // witness:passed
    events.push('first');
  })();
  const second = (async () => {
    assert.equal(await operand('second operand'), 4); // witness:passed
    events.push('second');
    release(4);
  })();
  await Promise.all([first, second]);
  trace('interleaved');
});

test('caught assertion failure keeps its native error', async () => {
  try {
    assert.equal(await operand('failure'), 999); // witness:failed
    throw new Error('missing failure');
  } catch (error) {
    if (error.code !== 'ERR_ASSERTION' || error.actual !== 4 || error.expected !== 999)
      throw error;
    events.push(error.code);
  }
  trace('failed');
});

test('rejected operand never invokes an assertion', async () => {
  const sentinel = new Error('operand rejection');
  try {
    assert.equal(await Promise.reject(sentinel), 4);
    throw new Error('missing rejection');
  } catch (error) {
    if (error !== sentinel) throw error;
    events.push('same rejection');
  }
  trace('rejected');
});

test('expected operand rejection does not create a witness', async () => {
  const sentinel = new Error('expected rejection');
  try {
    assert.equal(await operand('actual before rejection'), await Promise.reject(sentinel));
  } catch (error) {
    if (error !== sentinel) throw error;
    events.push('same expected rejection');
  }
  trace('expected rejection');
});

test('unbraced control flow and return keep their behavior', async () => {
  for (const n of [4, 4])
    if (n) assert.equal(await operand('loop'), n); // witness:passed:2
  if (false) assert.equal(await operand('unreachable'), 4);
  async function checked() {
    return assert.equal(await operand('returned'), 4); // witness:passed
  }
  const result = await checked();
  if (result !== undefined) throw new Error('changed async return');
  trace('control flow');
});

test('shadowed bindings are not assertion witnesses', async () => {
  async function custom(assert) {
    return assert.equal(await operand('shadowed'), 4);
  }
  const receiver = { equal(actual, expected) {
    if (this !== receiver) throw new Error('changed receiver');
    return actual + expected;
  } };
  if (await custom(receiver) !== 8) throw new Error('changed custom result');
  trace('shadowed');
});

test('a different test cannot borrow a suspended call site', async () => {
  assert.equal(await operand('separate attempt'), 4); // witness:passed
  trace('separate');
});
