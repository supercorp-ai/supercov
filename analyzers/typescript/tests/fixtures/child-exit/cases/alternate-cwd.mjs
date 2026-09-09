import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

test('the same entry string can execute another artifact', async () => {
  const first = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const completed = once(first, 'close');
  const other = spawn(process.execPath, ['src/cli.mjs'], {
    stdio: 'ignore', cwd: 'tests/alternate',
  });
  const exited = new Promise((resolve, reject) => {
    other.once('error', reject);
    other.once('exit', (code, signal) => resolve({ code, signal }));
  });
  await completed;
  assert.equal((await exited).code, 1);
});
