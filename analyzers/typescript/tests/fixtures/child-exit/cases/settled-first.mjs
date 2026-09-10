import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

test('an earlier resolution makes the exit callback irrelevant', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const completed = once(child, 'close');
  const exited = new Promise((resolve, reject) => {
    resolve({ code: 1, signal: null });
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  await completed;
  assert.equal((await exited).code, 1);
});
