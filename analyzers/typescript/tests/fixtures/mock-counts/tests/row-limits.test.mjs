import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createLogger } from '../src/factories.mjs';

const aliases = [{ label: 'alias', enabled: true, expected: 1 }];
const escaped = aliases;
for (const { label, enabled, expected } of aliases) {
  test(`escaped row ${label}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const logger = createLogger({ enabled, stream: 'log' });
    logger.info('alias payload');
    assert.equal(log.mock.callCount(), expected);
  });
}

const duplicates = [
  { label: 'same', enabled: true, expected: 1 },
  { label: 'same', enabled: false, expected: 0 },
];
for (const { label, enabled, expected } of duplicates) {
  test(`duplicate row ${label}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const logger = createLogger({ enabled, stream: 'log' });
    logger.info('duplicate payload');
    assert.equal(log.mock.callCount(), expected);
  });
}

const transforms = [{ label: 'transform', enabled: true, expected: 1 }];
for (const { label, enabled, expected } of transforms) {
  test(`transformed row ${label.toUpperCase()}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const logger = createLogger({ enabled, stream: 'log' });
    logger.info('transform payload');
    assert.equal(log.mock.callCount(), expected);
  });
}

const mutations = [{ label: 'mutated', enabled: true, expected: 0 }];
mutations[0].enabled = false;
for (const { label, enabled, expected } of mutations) {
  test(`mutated row ${label}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const logger = createLogger({ enabled, stream: 'log' });
    logger.info('mutated payload');
    assert.equal(log.mock.callCount(), expected);
  });
}

const extraStatements = [{ label: 'extra', enabled: true, expected: 1 }];
for (const { label, enabled, expected } of extraStatements) {
  const alias = enabled;
  test(`extra row ${label}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    const logger = createLogger({ enabled: alias, stream: 'log' });
    logger.info('extra payload');
    assert.equal(log.mock.callCount(), expected);
  });
}
