import { expect, test } from 'vitest';
import { label } from '../../lib/client';
import { echo } from '../../simulators/echo';

test('label names many', () => {
  expect(label(2, true)).toBe('many');
});

test('echo simulator returns its input', () => {
  expect(echo('a')).toBe('a');
});

test('the ignored environment file reached the tests', () => {
  expect(process.env.FIXTURE_SECRET).toBe('from-an-ignored-file');
});
