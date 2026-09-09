import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLogger } from '../src/factories.mjs';

const rows = [
  { label: 'c', enabled: true, stream: 'log', logCount: 1, errorCount: 0 },
  { label: 'a', enabled: false, stream: 'log', logCount: 0, errorCount: 0 },
  { label: 'b', enabled: true, stream: 'error', logCount: 0, errorCount: 1 },
]
for (const { label, enabled, stream, logCount, errorCount } of rows) {
  test(`source row ${label}: ${enabled}/${stream}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const error = t.mock.method(console, 'error', () => {});
    const logger = createLogger({ enabled, stream });
    logger.info('table payload');
    assert.equal(log.mock.callCount(), logCount);
    assert.equal(error.mock.callCount(), errorCount);
  });
}
