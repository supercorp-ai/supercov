// What a page ran before it navigated away is the test's.
//
//   node scripts/leaving-page.mjs
//
// A click handler that ends by setting `location.href` leaves the document
// it ran in. A page stores its evidence in localStorage as it leaves, and the
// next document restores it, but only on the same origin: an admin's
// masquerade link that sent the page from 127.0.0.1 to localhost lost every
// hit the handler made, though the test passes only if it ran. Both a page
// that stays on its origin and one that leaves it must keep the handler.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `leaving-page-${process.pid}-${Date.now()}`);
const port = 42000 + Math.floor(Math.random() * 3000);

const files = {
  'package.json': '{ "name": "leaving-page", "private": true, "type": "module" }\n',
  'server.mjs': `import { createServer } from 'node:http';\nimport { readFileSync } from 'node:fs';\n\ncreateServer((request, response) => {\n  const path = new URL(request.url, 'http://x').pathname;\n  const file = path === '/' ? 'index.html' : path.slice(1);\n  try {\n    const body = readFileSync(new URL(\`./public/\${file}\`, import.meta.url));\n    response.writeHead(200, { 'content-type': file.endsWith('.js') ? 'text/javascript' : 'text/html' });\n    response.end(body);\n  } catch {\n    response.writeHead(404);\n    response.end();\n  }\n}).listen(${port});\n`,
  'public/index.html': '<!doctype html><button id="same">same</button><button id="away">away</button><script type="module" src="/app.js"></script>\n',
  'public/done.html': '<!doctype html><p>done</p><script type="module" src="/app.js"></script>\n',
  'public/app.js': "export function leaveFor(target) {\n  const marker = target.length;\n  if (marker > 0) {\n    window.location.href = target;\n  }\n}\n\nexport function leaveForAnotherOrigin(target) {\n  const marker = target.length;\n  if (marker > 0) {\n    window.location.href = target;\n    document.title = 'leaving';\n  }\n}\n\ndocument.getElementById('same')?.addEventListener('click', () => leaveFor('/done.html'));\ndocument.getElementById('away')?.addEventListener('click', () => leaveForAnotherOrigin(location.href.replace('127.0.0.1', 'localhost') + 'done.html'));\n",
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${port}', browserName: 'chromium' },\n  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },\n};\n`,
  'tests/leave.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('leaves for the same origin', async ({ page }) => {\n  await page.goto('/');\n  await page.click('#same');\n  await page.waitForURL(/127\\.0\\.0\\.1.*done/);\n  await expect(page.locator('p')).toHaveText('done');\n});\n\ntest('leaves for another origin', async ({ page }) => {\n  await page.goto('/');\n  await page.click('#away');\n  await page.waitForURL(/localhost.*done/);\n  await expect(page.locator('p')).toHaveText('done');\n});\n",
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_SOURCE_ROOTS: 'public' });
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  // Line 12 runs after the assignment that starts the navigation.
  for (const [title, line] of [['leaves for the same origin', 4], ['leaves for another origin', 11], ['leaves for another origin', 12]]) {
    const covered = run(binary, ['runs', runId, 'line', `public/app.js:${line}`]);
    assert.equal(covered.status, 0, covered.stdout + covered.stderr);
    assert.match(covered.stdout, new RegExp(title, 'u'), `${title}: the handler that navigated away is not the test's\n${covered.stdout}`);
  }
  console.log('[leaving-page] a handler that sends the page away, to its own origin or another, stays the test\'s');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'leaving-page-'))) rmSync(root, { recursive: true, force: true });
}
