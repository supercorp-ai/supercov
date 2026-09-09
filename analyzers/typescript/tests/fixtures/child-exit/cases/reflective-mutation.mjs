import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('reflective access is not a closed result carrier', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  (eval)('exited.then((result) => { result.code = 1; })');
  assert.equal((await exited).code, 1);
});
