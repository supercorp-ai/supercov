import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

function launch() {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  return { child, exited };
}

test('local helper preserves the selected child code', async () => {
  const gateway = launch();
  assert.equal((await gateway.exited).code, 1);
});
