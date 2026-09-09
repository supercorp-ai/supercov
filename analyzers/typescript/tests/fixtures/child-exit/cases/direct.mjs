import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('local Promise preserves the selected child code', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  assert.equal((await exited).code, 1);
});
