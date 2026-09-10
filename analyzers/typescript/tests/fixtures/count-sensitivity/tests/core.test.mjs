import { test } from 'node:test';
import assert from 'node:assert/strict';
import { selectLogger } from '../src/logger.mjs';
const cases = [
  { mode: 'normal', destination: 'network', stdoutCount: 1, stderrCount: 1 },
  { mode: 'normal', destination: 'terminal', stdoutCount: 0, stderrCount: 2 },
  { mode: 'verbose', destination: 'network', stdoutCount: 1, stderrCount: 1 },
  { mode: 'verbose', destination: 'terminal', stdoutCount: 0, stderrCount: 2 },
  { mode: 'quiet', destination: 'network', stdoutCount: 0, stderrCount: 0 },
  { mode: 'quiet', destination: 'terminal', stdoutCount: 0, stderrCount: 0 },
];
for (const { mode, destination, stdoutCount, stderrCount } of cases) {
  test(`${mode}/${destination}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const error = t.mock.method(console, 'error', () => {});
    const logger = selectLogger({ mode, destination });
    logger.info('hello', { a: 1 });
    logger.error('oops');
    // observes: src/logger.mjs#selectLogger mode === 'quiet'; check count
    // observes: src/logger.mjs#selectLogger return destination === 'terminal' ? verboseStderr : verbose; check count
    // observes: src/logger.mjs#selectLogger return destination === 'terminal' ? normalStderr : normal; check count
    assert.equal(log.mock.callCount(), stdoutCount);
    // observes: src/logger.mjs#selectLogger mode === 'quiet'; check count
    assert.equal(error.mock.callCount(), stderrCount);
  });
}
