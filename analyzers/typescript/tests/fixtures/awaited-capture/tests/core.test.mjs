import { test } from 'node:test';
import assert from 'node:assert/strict';
import { openWorker as createChild } from './process.mjs';

test('awaited child capture', async t => {
  const processHandle = createChild(t);
  // observes: src/cli.mjs#start Listening on port
  await processHandle.settled();
  assert.match(processHandle.text(), /Listening/);
});

test('second caller of the same helper', async t => {
  const differentlyNamed = createChild(t);
  // observes: src/cli.mjs#start Listening on port
  await differentlyNamed.settled();
  assert.ok(differentlyNamed.text().length > 0);
});

test('invalid pragma is analysis only', async t => {
  const handle = createChild(t);
  // observes: src/missing.mjs#missing
  await handle.settled();
  assert.match(handle.text(), /Listening/);
});
