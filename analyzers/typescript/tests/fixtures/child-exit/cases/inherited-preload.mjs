import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('an inherited exit hook remaps the real producer result', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], {
    stdio: 'ignore',
    env: { ...process.env, NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ''} --import=./tests/exit-hook.mjs` },
  });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  assert.equal((await exited).code, 1);
});
