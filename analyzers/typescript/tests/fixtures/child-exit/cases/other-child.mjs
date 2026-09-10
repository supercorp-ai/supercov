import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

test('another child cannot protect the covered producer', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const completed = once(child, 'close');
  const other = spawn(process.execPath, ['-e', 'process.exit(1)'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    other.once('error', reject);
    other.once('exit', (code, signal) => resolve({ code, signal }));
  });
  await completed;
  assert.equal((await exited).code, 1);
});
