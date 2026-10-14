// Measuring must not change what a page can fetch from another origin.
//
//   node scripts/cross-origin-requests.mjs
//
// An app page fetches from a second origin whose CORS answer allows only
// `Content-Type`, as an embed's API often does. A request with any other
// header needs a preflight that origin refuses, so a coverage header added
// to it makes the request fail under Supercov while it succeeds without.
// Same-origin requests must still carry the test's scope: the app's server
// is measured, and its evidence has to land on the test that caused it.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `cross-origin-${process.pid}-${Date.now()}`);
const app = 42000 + Math.floor(Math.random() * 3000);
const api = app + 3000;

const files = {
  'package.json': '{ "name": "cross-origin", "private": true, "type": "module" }\n',
  'src/greeting.js': "export function greeting(scoped) {\n  if (scoped) return 'scoped';\n  return 'unscoped';\n}\n\nexport function navigated(scoped) {\n  if (scoped) return 'scoped';\n  return 'unscoped';\n}\n",
  // The app: a page, and an endpoint of its own that says whether the
  // request carried the test's scope (header or cookie).
  'app.mjs': `import { createServer } from 'node:http';\nimport { greeting, navigated } from './src/greeting.js';\n\nconst page = \`<!doctype html><p id="api">pending</p><p id="own">pending</p><p id="xhr">pending</p><script>\nfetch('http://127.0.0.1:${api}/articles').then((r) => r.json()).then((d) => { document.querySelector('#api').textContent = d.articles; }, (e) => { document.querySelector('#api').textContent = 'blocked'; });\nfetch('/own').then((r) => r.text()).then((t) => { document.querySelector('#own').textContent = t; });\nconst x = new XMLHttpRequest(); x.open('GET', 'http://127.0.0.1:${api}/articles'); x.onload = () => { document.querySelector('#xhr').textContent = JSON.parse(x.responseText).articles; }; x.onerror = () => { document.querySelector('#xhr').textContent = 'blocked'; }; x.send();\n</script>\`;\ncreateServer((request, response) => {\n  if (request.url === '/ready') {\n    response.end('ready');\n    return;\n  }\n  const scoped = Boolean(request.headers['x-supercov-scope'] || /__supercov_scope=/.test(request.headers.cookie ?? ''));\n  if (request.url === '/own') {\n    response.end(greeting(scoped));\n    return;\n  }\n  navigated(scoped);\n  response.writeHead(200, { 'content-type': 'text/html' });\n  response.end(page);\n}).listen(${app}, '127.0.0.1');\n`,
  // Another origin: answers a simple request, refuses any extra header.
  'api.mjs': `import { createServer } from 'node:http';\n\ncreateServer((request, response) => {\n  const cors = { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'Content-Type' };\n  if (request.method === 'OPTIONS') {\n    const asked = (request.headers['access-control-request-headers'] ?? '').split(',').map((h) => h.trim().toLowerCase()).filter(Boolean);\n    response.writeHead(asked.every((h) => h === 'content-type') ? 204 : 403, cors);\n    response.end();\n    return;\n  }\n  response.writeHead(200, { ...cors, 'content-type': 'application/json' });\n  response.end(JSON.stringify({ articles: 'loaded' }));\n}).listen(${api}, '127.0.0.1');\n`,
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${app}', browserName: 'chromium' },\n  webServer: [\n    { command: 'node app.mjs', url: 'http://127.0.0.1:${app}/ready', reuseExistingServer: false },\n    { command: 'node api.mjs', url: 'http://127.0.0.1:${api}/articles', reuseExistingServer: false },\n  ],\n};\n`,
  'tests/widget.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('the widget loads articles from another origin', async ({ page }) => {\n  await page.goto('/');\n  await expect(page.locator('#api')).toHaveText('loaded');\n  await expect(page.locator('#xhr')).toHaveText('loaded');\n});\n\ntest('the app hears from its own page', async ({ page }) => {\n  await page.goto('/');\n  await expect(page.locator('#own')).not.toHaveText('pending');\n});\n",
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const plain = run('npx', ['playwright', 'test']);
  assert.equal(plain.status, 0, `the suite fails without Supercov:\n${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', 'npx', 'playwright', 'test']);
  assert.equal(measured.status, 0, `the suite passes without Supercov and fails with it:\n${measured.stdout}${measured.stderr}`);
  // The app's own requests still carried the test's scope: the branch that
  // runs only when it arrived ran, and was credited to the test.
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const byTest = run(binary, ['runs', runId, 'test', 'the app hears from its own page']);
  assert.equal(byTest.status, 0, byTest.stdout + byTest.stderr);
  assert.match(byTest.stdout, /src\/greeting\.js/u, byTest.stdout);
  // The page's first navigation, before any of its scripts ran, and its own
  // fetch: neither reached the app without the scope.
  const file = JSON.parse(run(binary, ['runs', runId, 'file', 'src/greeting.js', '--json']).stdout).data;
  const gaps = file.gapLines.map((gap) => gap.line);
  assert.ok(gaps.includes(3) && gaps.includes(8), `a request reached the app without the scope: ${JSON.stringify(file.gapLines)}`);
  console.log('[cross-origin] a page under measurement fetches from an origin that allows no extra headers, and its own requests stay scoped');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, "target", "cross-origin-"))) rmSync(root, { recursive: true, force: true });
}
