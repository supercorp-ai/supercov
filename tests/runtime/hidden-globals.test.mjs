import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

// Mocha's --check-leaks fails a test whose run adds an enumerable key to the
// global object. Supercov's runtime first sets some of its globals while a
// test runs: none of them may show among those keys.
test("the runtime's globals never show among the global object's keys", () => {
  const runtime = JSON.stringify(pathToFileURL(resolve(import.meta.dirname, '../../runtime/javascript/runtime.mjs')).href);
  const source = `
    const before = new Set(Object.keys(globalThis));
    const runtime = await import(${runtime});
    runtime.resetCoverage('a test');
    runtime.activateCoverageScope('a scope');
    runtime.coverageHit('a point');
    runtime.activateCoverageScope(undefined);
    const added = Object.keys(globalThis).filter(key => !before.has(key));
    console.log(JSON.stringify({ added, testId: globalThis.__SUPERCOV_MCDC_TEST_ID__ }));
  `;
  const ran = spawnSync(process.execPath, ['--input-type=module', '-e', source], { encoding: 'utf8' });
  assert.equal(ran.status, 0, ran.stderr);
  const { added, testId } = JSON.parse(ran.stdout.trim().split('\n').at(-1));
  assert.deepEqual(added.filter(key => key.startsWith('__SUPERCOV_')), []);
  // Still set and read as before.
  assert.equal(testId, 'a test');
});
