// A header a test sets on its browser context reaches the app.
//
//   node scripts/context-headers.mjs
//
// A test sets `X-Forwarded-For` with `context.setExtraHTTPHeaders`. Supercov
// set the context's headers back to those it was created with at every step,
// so after the first `expect` the app saw no such header under measurement,
// though it did without Supercov.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `context-headers-${process.pid}-${Date.now()}`);

const files = {
  'package.json': '{ "name": "context-headers", "private": true, "type": "module" }\n',
  'src/client.js': 'export const forwarded = (headers) => headers["x-forwarded-for"] ?? "none";\n',
  'playwright.config.js': "export default { testDir: 'tests', reporter: 'line', use: { browserName: 'chromium' } };\n",
  'tests/headers.spec.js': `import http from 'node:http';
import { expect, test } from '@playwright/test';
import { forwarded } from '../src/client.js';

test('a header the test sets reaches the app', async ({ page, context }) => {
  const seen = [];
  const server = http.createServer((request, response) => {
    seen.push(forwarded(request.headers));
    response.end('<p>ok</p>');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const url = \`http://127.0.0.1:\${server.address().port}/\`;
  await context.setExtraHTTPHeaders({ 'x-forwarded-for': '203.0.113.7' });
  await page.goto(url);
  await expect(page.locator('p')).toHaveText('ok');
  await page.goto(url);
  server.close();
  expect(seen).toEqual(['203.0.113.7', '203.0.113.7']);
});
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const plain = run('npx', ['playwright', 'test']);
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', 'npx', 'playwright', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  console.log('[context-headers] a header a test sets on its context reaches the app under measurement, as without it');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'context-headers-'))) rmSync(root, { recursive: true, force: true });
}
