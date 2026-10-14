// A request a test causes indirectly is credited to that test.
//
//   node scripts/background-by-time.mjs
//
// The app's webhook is called by a mock payment server, a separate process
// that carries no test identity, as a Stripe mock does: the test asks the
// app to check out, the app asks the mock, and the mock calls the app's
// webhook on its own. The webhook's code ran while one test, and only one,
// was running, so it is that test's -- credited by time, and said to be.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `background-by-time-${process.pid}-${Date.now()}`);
const app = 45000 + Math.floor(Math.random() * 2000);
const stripe = app + 2000;

const files = {
  'package.json': '{ "name": "background-by-time", "private": true, "type": "module" }\n',
  'src/webhook.js': "export function settle(event) {\n  if (event.type === 'charge.succeeded') return 'paid';\n  return 'ignored';\n}\n",
  'src/server.js': `import { createServer } from 'node:http';\nimport { settle } from './webhook.js';\n\nlet state = 'none';\ncreateServer((request, response) => {\n  if (request.url === '/ready') return response.end('ready');\n  if (request.url === '/state') return response.end(state);\n  if (request.url === '/checkout') {\n    state = 'pending';\n    fetch('http://127.0.0.1:${stripe}/charge', { method: 'POST' }).then(() => response.end('started'));\n    return;\n  }\n  if (request.url === '/webhook') {\n    let body = '';\n    request.on('data', (chunk) => (body += chunk));\n    request.on('end', () => { state = settle(JSON.parse(body)); response.end('ok'); });\n    return;\n  }\n  response.end('?');\n}).listen(${app}, '127.0.0.1');\n`,
  // The mock: answers the charge, then calls the app's webhook by itself.
  'mocks/stripe.mjs': `import { createServer } from 'node:http';\n\ncreateServer((request, response) => {\n  if (request.url === '/charge') {\n    response.end('{}');\n    setTimeout(() => fetch('http://127.0.0.1:${app}/webhook', { method: 'POST', body: JSON.stringify({ type: 'charge.succeeded' }) }), 100);\n    return;\n  }\n  response.end('ready');\n}).listen(${stripe}, '127.0.0.1');\n`,
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${app}' },\n  webServer: [\n    { command: 'node src/server.js', url: 'http://127.0.0.1:${app}/ready', reuseExistingServer: false },\n    { command: 'node mocks/stripe.mjs', url: 'http://127.0.0.1:${stripe}/', reuseExistingServer: false },\n  ],\n};\n`,
  'tests/checkout.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a checkout is paid when the webhook arrives', async ({ request }) => {\n  await request.get('/checkout');\n  await expect.poll(async () => (await request.get('/state')).text()).toBe('paid');\n});\n\ntest('the state can be read', async ({ request }) => {\n  expect(await (await request.get('/state')).text()).toBeTruthy();\n});\n",
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_SOURCE_ROOTS: 'src' });
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const paid = run(binary, ['runs', runId, 'test', 'a checkout is paid when the webhook arrives']);
  assert.equal(paid.status, 0, paid.stdout + paid.stderr);
  if (process.env.SHOW) console.log(paid.stdout, run(binary, ['runs', runId]).stdout);
  assert.match(paid.stdout, /src\/webhook\.js/u, `the webhook is not the checkout test's:\n${paid.stdout}`);
  assert.match(paid.stdout, /credited by time/u, `the test does not say how it was credited:\n${paid.stdout}`);
  // The other test ran no webhook, and start-up stays no test's.
  const read = run(binary, ['runs', runId, 'test', 'the state can be read']);
  assert.doesNotMatch(read.stdout, /webhook\.js|credited by time/u, read.stdout);
  const summary = run(binary, ['runs', runId]).stdout;
  assert.match(summary, /Attribution {6}1 exact, 1 with server work credited by time/u, summary);
  assert.match(summary, /no test {20}lines/u, summary);
  console.log('[background-by-time] a webhook another process sends is credited, by time, to the test running then');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'background-by-time-'))) rmSync(root, { recursive: true, force: true });
}
