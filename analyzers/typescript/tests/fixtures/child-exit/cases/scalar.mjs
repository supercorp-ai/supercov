import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';

test('resolver callback preserves the first event argument', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const code = await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  });
  assert.equal(code, 1);
});
