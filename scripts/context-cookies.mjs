// What a test reads of its browser context's cookies is its own.
//
//   node scripts/context-cookies.mjs
//
// Supercov carries a test's scope to the app in cookies. A test that read
// `context.cookies()` saw them and `storageState({ path })` wrote them into
// the project's saved auth state. A `clearCookies()` removes them, and the
// test's next action must put them back.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `context-cookies-${process.pid}-${Date.now()}`);
const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});
const port = await freePort();

const files = {
  'package.json': '{ "name": "context-cookies", "private": true, "type": "module" }\n',
  'server.mjs': `import { appendFileSync } from 'node:fs';
import { createServer } from 'node:http';

export function answer(path) {
  if (path === '/api/after-clear') {
    return 'cleared';
  }
  return '<p>ok</p>';
}

createServer((request, response) => {
  const path = new URL(request.url, 'http://x').pathname;
  if (path === '/api/after-clear') {
    // Whether the request still carried a test's scope, for the gate.
    appendFileSync('after-clear.log', /__supercov_scope=/.test(request.headers.cookie ?? '') ? 'scoped\\n' : 'unscoped\\n');
  }
  response.end(answer(path));
}).listen(${port});
`,
  'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}', browserName: 'chromium' },
  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },
};
`,
  'tests/cookies.spec.js': `import { expect, test } from '@playwright/test';

test('the cookies a test reads are its own', async ({ page, context }) => {
  await page.goto('/');
  await expect(page.locator('p')).toHaveText('ok');
  await context.addCookies([{ name: 'theme', value: 'dark', url: 'http://127.0.0.1:${port}' }]);
  expect((await context.cookies()).map((cookie) => cookie.name)).toEqual(['theme']);
  const state = await context.storageState({ path: 'state.json' });
  expect(state.cookies.map((cookie) => cookie.name)).toEqual(['theme']);
  await context.clearCookies();
  expect(await context.cookies()).toEqual([]);
  // A navigation carries the scope in cookies alone.
  await page.goto('/api/after-clear');
  await expect(page.locator('body')).toHaveText('cleared');
});
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', SUPERCOV_SOURCE_ROOTS: '.' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const plain = run('npx', ['playwright', 'test']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  rmSync(resolve(root, 'state.json'), { force: true });
  const measured = run(binary, ['--', 'npx', 'playwright', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  assert.equal(readFileSync(resolve(root, 'after-clear.log'), 'utf8').trim().split('\n').at(-1), 'scoped', 'the next action after clearCookies carries the scope');
  const saved = JSON.parse(readFileSync(resolve(root, 'state.json'), 'utf8'));
  assert.deepEqual(saved.cookies.map((cookie) => cookie.name), ['theme'], 'the saved state holds the suite\'s cookies only');
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const line = run(binary, ['runs', runId, 'line', 'server.mjs:5', '--json']);
  assert.equal(line.status, 0, line.stdout + line.stderr);
  const tests = JSON.parse(line.stdout).data.tests.map((test) => test.name.split(' > ').at(-1));
  assert.ok(tests.includes('the cookies a test reads are its own'), `the request after clearCookies is the test's: ${line.stdout}`);
  console.log('[context-cookies] a test reads, saves and clears only its own cookies, and what the app does after a clear is still the test\'s');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'context-cookies-'))) rmSync(root, { recursive: true, force: true });
}
