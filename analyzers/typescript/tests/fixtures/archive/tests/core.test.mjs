import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { exact, discarded, coerced, sendStatus, unchecked, choose } from '../src/core.mjs';
import { twice } from '../src/typed.ts';
import { overwritten, ignoredCallback, caught, sameLine, retainedAlias } from '../src/core.mjs';

test('TypeScript return through ordinary source loading', () => {
  assert.equal(twice(3), 6);
});

test('exact return', () => {
  // observes: src/core.mjs#exact return 4
  assert.equal(exact(), 4);
});
test('discarded while evaluating assertion', () => {
  // observes: src/core.mjs#discarded return 4 via comma expression
  assert.equal((discarded(), 7), 7);
});
test('coercion loses distinctions', () => {
  assert.equal(Boolean(coerced()), true);
});
test('a helper name is not its implementation', async () => {
  function fetch() { return { status: 200 }; }
  sendStatus({ status() {} });
  const response = await fetch();
  assert.equal(response.status, 200);
});
test('an assertion present in source but never run', () => {
  unchecked();
  if (false) assert.equal(unchecked(), 4);
});
test('decision true', () => { assert.equal(choose(true), 'yes'); });
test('decision false', () => { assert.equal(choose(false), 'no'); });
test('overwrite loses the original return', () => {
  let value = overwritten();
  value = 7;
  assert.equal(value, 7);
});
test('a callback result can be ignored', () => {
  function ignore(callback) { callback(); return 7; }
  assert.equal(ignore(ignoredCallback), 7);
});
test('caught failing assertion is not a passing oracle', () => {
  try {
    // observes: src/core.mjs#caught return 4
    assert.equal(caught(), 999);
  } catch {}
});
test('same line is not the same assertion', () => {
  sameLine(); if (false) assert.equal(sameLine(), 4); assert.equal(7, 7);
});
test('constant alias and right comma operand are still checked', () => {
  const value = (ignoredCallback(), retainedAlias());
  assert.equal(value, 4);
});
test('real subprocess exit and close-delimited pipe capture', async () => {
  const child = spawn(process.execPath, ['src/cli.mjs'], { stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  child.stderr.on('data', chunk => { output += chunk; });
  const code = await new Promise((resolve, reject) => {
    child.on('error', reject);
    child.on('close', resolve);
  });
  assert.equal(code, 3);
  assert.ok(output.includes('token:'));
});

test('pragma cannot borrow a different assertion', () => {
  exact();
  // observes: src/core.mjs#exact return 4
  assert.equal(7, 7);
  assert.equal(exact(), 4);
});
test('pragma preserves presence strength', () => {
  // observes: src/core.mjs#exact return 4
  assert.ok(exact());
});
test('pragma target diagnostics', () => {
  // observes: src/core.mjs#missing
  assert.equal(exact(), 4);
  // observes: src/core.mjs#choose
  assert.equal(choose(true), 'yes');
  // observes: src/core.mjs
  assert.equal(exact(), 4);
  // observes: src/core.mjs#exact return 4
  const ignored = exact();
  assert.equal(ignored, 4);
  // observes: src/core.mjs#exact return 4
  (assert.equal(exact(), 4), assert.equal(7, 7));
});
test('pragma still needs its exact passing call', () => {
  exact();
  if (false) {
    // observes: src/core.mjs#exact return 4
    assert.equal(exact(), 4);
  }
  for (const expected of [4, 999]) {
    try {
      // observes: src/core.mjs#exact return 4
      assert.equal(exact(), expected);
    } catch {}
  }
});
