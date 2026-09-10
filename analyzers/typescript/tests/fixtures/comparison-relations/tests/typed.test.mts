import { test } from 'node:test';
import assert from 'node:assert/strict';
import { typedSelf } from '../src/core.mjs';

test('erased TypeScript syntax preserves immutable aliases', () => {
  const actual = typedSelf();
  const expected = (actual as string)!;
  assert.deepStrictEqual((actual satisfies string), expected);
});
