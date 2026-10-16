// A server whose emit something else wrapped first still credits its tests.
//
//   node scripts/hidden-request-emit.mjs
//
// A preload put its own `emit` on `http.Server.prototype`, calling the
// EventEmitter emit that existed when it loaded. Supercov's patch of that
// emit, installed later, never saw the server's requests, and what ran on a
// request's own events (its body's `end`, a page rendered as the response
// went out) was credited to no test. Node publishes each request on
// `diagnostics_channel` as well, which no emit hides.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `hidden-request-emit-${process.pid}-${Date.now()}`);
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
  'package.json': '{ "name": "hidden-request-emit", "private": true, "type": "module" }\n',
  // As a test suite's clock mock did: an own emit over the native one.
  'preload/wrap-emit.cjs': `const http = require('node:http');
const { EventEmitter } = require('node:events');
const native = EventEmitter.prototype.emit;
http.Server.prototype.emit = function (event, ...args) {
  return native.call(this, event, ...args);
};
`,
  'src/server.mjs': `import { createServer } from 'node:http';

export function received(body) {
  return \`got \${body.length}\`;
}

createServer((request, response) => {
  let body = '';
  request.on('data', (chunk) => { body += chunk; });
  request.on('end', () => {
    response.end(received(body));
  });
}).listen(${port});
`,
  'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'node --require ./preload/wrap-emit.cjs src/server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },
};
`,
  'tests/post.spec.js': `import { expect, test } from '@playwright/test';

test('posting a body', async ({ request }) => {
  const response = await request.post('/submit', { data: 'hello' });
  expect(await response.text()).toBe('got 5');
});
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const measured = run(binary, ['--', 'npx', 'playwright', 'test']);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const line = run(binary, ['runs', runId, 'line', 'src/server.mjs:4', '--json']);
  assert.equal(line.status, 0, line.stdout + line.stderr);
  const tests = JSON.parse(line.stdout).data.tests.map((test) => test.name.split(' > ').at(-1));
  assert.ok(tests.includes('posting a body'), `the body read on 'end' is the test's: ${line.stdout}`);
  console.log('[hidden-request-emit] a request whose server emit was wrapped first is still its test\'s, body and all');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'hidden-request-emit-'))) rmSync(root, { recursive: true, force: true });
}
