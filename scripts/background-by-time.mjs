// A request a test causes indirectly is credited to that test.
//
//   node scripts/background-by-time.mjs
//
// The app's webhook is called by a mock payment server, a separate process
// Supercov does not measure (as a stripe-mock binary is), so the call
// carries no test identity: the test asks the app to check out, the app asks
// the mock, and the mock calls the app's webhook on its own. The webhook's
// code ran while one test, and only one, was running, so it is that test's
// -- credited by time, and said to be.
//
// And a request that names its test, whose handler reads the body: Node
// emits the body's events outside the handler, and that work was no test's.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer as createNetServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `background-by-time-${process.pid}-${Date.now()}`);
// Ports the system says are free: a random one in a fixed range met a
// process another gate left listening on a CI runner (EADDRINUSE).
const freePort = () =>
  new Promise((settle, fail) => {
    const server = createNetServer();
    server.once('error', fail);
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      server.close(() => settle(port));
    });
  });
const [app, stripe, ...workerPorts] = await Promise.all(Array.from({ length: 6 }, freePort));
const workers = [workerPorts.slice(0, 2), workerPorts.slice(2, 4)];

const files = {
  'package.json': '{ "name": "background-by-time", "private": true, "type": "module" }\n',
  'src/webhook.js': "export function settle(event) {\n  if (event.type === 'charge.succeeded') return 'paid';\n  return 'ignored';\n}\n",
  'src/note.js': "export function note(body) {\n  if (body.text) return body.text.trim();\n  return '';\n}\n",
  'src/server.js': `import { createServer } from 'node:http';\nimport { settle } from './webhook.js';\nimport { note } from './note.js';\n\nlet state = 'none';\ncreateServer((request, response) => {\n  if (request.url === '/ready') return response.end('ready');\n  if (request.url === '/note') {\n    let body = '';\n    request.on('data', (chunk) => (body += chunk));\n    request.on('end', () => response.end(note(JSON.parse(body))));\n    return;\n  }\n  if (request.url === '/state') return response.end(state);\n  if (request.url === '/checkout') {\n    state = 'pending';\n    fetch('http://127.0.0.1:' + (process.argv[3] ?? ${stripe}) + '/charge', { method: 'POST' }).then(() => response.end('started'));\n    return;\n  }\n  if (request.url === '/webhook') {\n    let body = '';\n    request.on('data', (chunk) => (body += chunk));\n    request.on('end', () => { state = settle(JSON.parse(body)); response.end('ok'); });\n    return;\n  }\n  response.end('?');\n}).listen(Number(process.argv[2] ?? ${app}), '127.0.0.1');\n`,
  // The mock: answers the charge, then calls the app's webhook by itself.
  'mocks/stripe.mjs': `import { createServer } from 'node:http';\n\ncreateServer((request, response) => {\n  if (request.url === '/charge') {\n    response.end('{}');\n    setTimeout(() => fetch('http://127.0.0.1:' + (process.argv[3] ?? ${app}) + '/webhook', { method: 'POST', body: JSON.stringify({ type: 'charge.succeeded' }) }), 100);\n    return;\n  }\n  response.end('ready');\n}).listen(Number(process.argv[2] ?? ${stripe}), '127.0.0.1');\n`,
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${app}' },\n  webServer: [\n    { command: 'node src/server.js', url: 'http://127.0.0.1:${app}/ready', reuseExistingServer: false },\n    // Started without Supercov, as an external mock binary would be.
    { command: 'env -u NODE_OPTIONS node mocks/stripe.mjs', url: 'http://127.0.0.1:${stripe}/', reuseExistingServer: false },\n  ],\n};\n`,
  // Two workers, each with its own app server and mock, as a suite that
  // starts a server per worker does; their checkouts overlap in time.
  'parallel/playwright.config.js': "export default { testDir: '.', workers: 2, fullyParallel: true, reporter: 'line' };\n",
  'parallel/fixture.js': `import { test as base } from '@playwright/test';\nimport { spawn } from 'node:child_process';\n\n// A measured process passes Supercov on to what it spawns; \`env -u\` is how\n// a mock binary Supercov does not measure is stood in for.\nconst started = (args, env, program = 'node') => new Promise((ready) => {\n  const child = spawn(program, args, { env, stdio: 'ignore' });\n  setTimeout(() => ready(child), 800);\n});\nexport const test = base.extend({\n  app: [async ({}, use, workerInfo) => {\n    const [port, mock] = ${JSON.stringify(workers)}[workerInfo.workerIndex];\n    const unmeasured = Object.fromEntries(Object.entries(process.env).filter(([name]) => name !== 'NODE_OPTIONS' && !name.startsWith('SUPERCOV_')));\n    unmeasured.PATH = (unmeasured.PATH ?? '').split(':').filter((entry) => !entry.includes('.supercov')).join(':');\n    // Measured, and owned by no test: started as a per-worker server is,\n    // outside any test's context.\n    const server = await started(['-u', 'SUPERCOV_CONTEXT', 'node', 'src/server.js', String(port), String(mock)], process.env, 'env');\n    const payments = await started(['-u', 'NODE_OPTIONS', 'node', 'mocks/stripe.mjs', String(mock), String(port)], unmeasured, 'env');\n    await use(\`http://127.0.0.1:\${port}\`);\n    server.kill('SIGTERM');\n    payments.kill('SIGTERM');\n    await new Promise((done) => setTimeout(done, 300));\n  }, { scope: 'worker' }],\n});\nexport { expect } from '@playwright/test';\n`,
  'parallel/one.spec.js': "import { expect, test } from './fixture.js';\n\ntest('worker one checks out', async ({ app, request }) => {\n  await request.get(`${app}/checkout`);\n  await expect.poll(async () => (await request.get(`${app}/state`)).text()).toBe('paid');\n  await new Promise((done) => setTimeout(done, 600));\n});\n",
  'parallel/two.spec.js': "import { expect, test } from './fixture.js';\n\ntest('worker two checks out', async ({ app, request }) => {\n  await request.get(`${app}/checkout`);\n  await expect.poll(async () => (await request.get(`${app}/state`)).text()).toBe('paid');\n  await new Promise((done) => setTimeout(done, 600));\n});\n",
  'tests/checkout.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a checkout is paid when the webhook arrives', async ({ request }) => {\n  await request.get('/checkout');\n  await expect.poll(async () => (await request.get('/state')).text()).toBe('paid');\n});\n\ntest('the state can be read', async ({ request }) => {\n  expect(await (await request.get('/state')).text()).toBeTruthy();\n});\n\ntest('a note is kept', async ({ request }) => {\n  expect(await (await request.post('/note', { data: { text: ' hello ' } })).text()).toBe('hello');\n});\n",
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
  // The body a test's own request carried was read under that test.
  const kept = run(binary, ['runs', runId, 'test', 'a note is kept']);
  assert.match(kept.stdout, /src\/note\.js/u, `the note handler's body work is not the test's:\n${kept.stdout}`);
  assert.doesNotMatch(kept.stdout, /credited by time/u, kept.stdout);
  const summary = run(binary, ['runs', runId]).stdout;
  assert.match(summary, /Attribution {6}2 exact, 1 with server work credited by time/u, summary);
  assert.match(summary, /no test {20}lines/u, summary);
  // Two workers at once, each with its own server: each server's webhook is
  // its own worker's test's, though the tests overlap.
  const parallel = run(binary, ['--', 'npx', 'playwright', 'test', '--config', 'parallel/playwright.config.js'], { SUPERCOV_SOURCE_ROOTS: 'src' });
  assert.equal(parallel.status, 0, `${parallel.stdout}${parallel.stderr}`);
  const parallelRun = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  for (const name of ['worker one checks out', 'worker two checks out']) {
    const page = run(binary, ['runs', parallelRun, 'test', name]).stdout;
    assert.match(page, /src\/webhook\.js/u, `${name}: the webhook is not its test's:\n${page}`);
    assert.match(page, /credited by time/u, page);
  }
  console.log('[background-by-time] a webhook an unmeasured process sends is credited, by time, to the test running then; a request body is read under its test');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'background-by-time-'))) rmSync(root, { recursive: true, force: true });
}
