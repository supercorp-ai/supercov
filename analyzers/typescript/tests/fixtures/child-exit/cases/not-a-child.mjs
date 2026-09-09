import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';

test('an exit-named event is not a native child exit', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: 'ignore' });
  const completed = once(child, 'close');
  const emitter = new EventEmitter();
  const exited = new Promise((resolve) => {
    emitter.once('exit', (code, signal) => resolve({ code, signal }));
    emitter.emit('exit', 1, null);
  });
  await completed;
  assert.equal((await exited).code, 1);
});
