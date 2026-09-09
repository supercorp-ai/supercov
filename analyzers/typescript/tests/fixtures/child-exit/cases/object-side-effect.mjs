import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('another object field can transform the event parameter', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ sideEffect: (code = 1), code, signal }));
  });
  assert.equal((await exited).code, 1);
});
