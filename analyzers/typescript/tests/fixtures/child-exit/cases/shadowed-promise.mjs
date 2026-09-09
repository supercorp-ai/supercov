import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

test('a Promise-shaped constructor need not carry its executor result', async () => {
  class Promise {
    constructor(executor) { executor(() => {}, () => {}); }
    then(resolve) { resolve({ code: 1, signal: null }); }
  }
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const completed = once(child, 'close');
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  await completed;
  assert.equal((await exited).code, 1);
});
