import { test } from 'node:test';
import assert from 'node:assert/strict';
import { selectLogger } from '../src/logger.mjs';
const cases = [
  { mode: 'normal', destination: 'network' },
  { mode: 'normal', destination: 'terminal' },
  { mode: 'verbose', destination: 'network' },
  { mode: 'verbose', destination: 'terminal' },
];
for (const { mode, destination } of cases) {
  test(`${mode}/${destination}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const error = t.mock.method(console, 'error', () => {});
    const logger = selectLogger({ mode, destination });
    logger.info('hello', { a: 1 });
    logger.error('oops');
    const infoCalls = destination === 'terminal' ? error.mock.calls.slice(0, 1) : log.mock.calls;
    assert.equal(log.mock.callCount(), destination === 'terminal' ? 0 : 1);
    assert.equal(error.mock.callCount(), destination === 'terminal' ? 2 : 1);
    // observes: src/logger.mjs#formatData args.map; check value
    assert.equal(infoCalls[0].arguments[0], 'prefix');
    // observes: src/logger.mjs#formatData args.map; check value
    assert.equal(infoCalls[0].arguments[1], 'hello');
    if (mode === 'normal') {
      // observes: src/logger.mjs#selectLogger mode === 'verbose'; check value
      assert.deepEqual(infoCalls[0].arguments[2], { a: 1 });
    }
  });
}
