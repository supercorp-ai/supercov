import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';

function launch(t) {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  t.after(async () => {
    await Promise.race([exited, delay(10)]);
    await exited;
  });
  return { child, exited, output: () => 'a diagnostic' };
}

test('discarded teardown waits and a sibling reader preserve the result', async (t) => {
  const gateway = launch(t);
  gateway.output();
  assert.equal((await gateway.exited).code, 1);
});
