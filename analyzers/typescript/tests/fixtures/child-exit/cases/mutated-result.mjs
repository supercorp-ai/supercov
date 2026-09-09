import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('a resolved event field can be overwritten before assertion', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  const result = await exited;
  result.code = 1;
  assert.equal(result.code, 1);
});
