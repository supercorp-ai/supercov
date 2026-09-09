import test from 'node:test';
import assert from 'node:assert/strict';
import { launch } from './launch.mjs';

test('imported helper preserves the selected child code', async () => {
  const gateway = launch();
  assert.equal((await gateway.exited).code, 1);
});
