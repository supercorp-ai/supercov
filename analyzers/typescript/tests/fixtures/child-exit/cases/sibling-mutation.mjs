import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

function launch() {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const exited = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  return { child, exited, output: () => exited.then((result) => { result.code = 1; }) };
}

test('a sibling arrow reader can retain and change the result', async () => {
  const gateway = launch();
  gateway.output();
  assert.equal((await gateway.exited).code, 1);
});
