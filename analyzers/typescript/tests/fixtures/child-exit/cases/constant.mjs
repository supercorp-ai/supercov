import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('a constant result discards the exit code', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code: 1, signal }));
  });
  assert.equal((await exited).code, 1);
});
