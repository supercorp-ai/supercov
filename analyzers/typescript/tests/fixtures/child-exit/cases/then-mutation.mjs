import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('another Promise consumer changes the result before the read', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  exited.then((result) => { result.code = 1; });
  assert.equal((await exited).code, 1);
});
