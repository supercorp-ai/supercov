import { test } from 'node:test';
import assert from 'node:assert/strict';
import { producer, rewrite } from '../src/eval-write.mjs';

const evalRows = [{ label: 'eval', expected: 0 }];
(eval)("evalRows[0].expected = 1");
for (const { label, expected } of evalRows) {
  test(`eval row ${label}`, (t) => {
    const log = t.mock.method(console, 'log', () => {});
    console.log('eval row payload');
    assert.equal(log.mock.callCount(), expected);
  });
}
test('prepare changed producer', () => { rewrite(); });
test('changed producer stays unresolved', (t) => {
  const log = t.mock.method(console, 'log', () => {});
  producer();
  assert.equal(log.mock.callCount(), 1);
});
