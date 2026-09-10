import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('the signal field is not the numeric exit code', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  assert.equal((await exited).signal, null);
});
