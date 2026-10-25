// A change to a mock server affects only the tests that server served.
//
//   node scripts/mock-reach.mjs
//
// A Playwright suite starts two mock servers through `webServer`, outside
// what is measured: one reads a data file through a helper of its own. Each
// test talks to one of them. A change to the first mock's data file affects
// only the test it served (`tests affected`), and measuring that test alone
// then merging gives the suite's run, with the test's old record left out.
// Merging without measuring it again is refused, and names it.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `mock-reach-${process.pid}-${Date.now()}`);
const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});
const pay = await freePort();
const mail = await freePort();

const files = {
  'package.json': '{ "name": "mock-reach", "private": true, "type": "module" }\n',
  'src/price.js': "export function price(amount) {\n  return amount > 100 ? 'large' : 'small';\n}\n",
  'tests/mocks/pay-api/answers.json': '{ "status": "paid" }\n',
  'tests/mocks/pay-api/answer.mjs': "import { readFileSync } from 'node:fs';\nexport function answer() {\n  return JSON.parse(readFileSync(new URL('./answers.json', import.meta.url), 'utf8')).status;\n}\n",
  'tests/mocks/pay.mjs': "import { createServer } from 'node:http';\nimport { answer } from './pay-api/answer.mjs';\ncreateServer((request, response) => response.end(request.url === '/ready' ? 'ready' : answer())).listen(Number(process.env.PORT));\n",
  'tests/mocks/mail.mjs': "import { createServer } from 'node:http';\ncreateServer((request, response) => response.end(request.url === '/ready' ? 'ready' : 'sent')).listen(Number(process.env.PORT));\n",
  'playwright.config.js': `export default {
  testDir: 'tests/e2e',
  workers: 1,
  reporter: 'line',
  webServer: [
    { command: 'node tests/mocks/pay.mjs', env: { PORT: '${pay}' }, url: 'http://127.0.0.1:${pay}/ready', reuseExistingServer: false },
    { command: 'node tests/mocks/mail.mjs', env: { PORT: '${mail}' }, url: 'http://127.0.0.1:${mail}/ready', reuseExistingServer: false },
  ],
};
`,
  'tests/e2e/pay.spec.js': `import { expect, test } from '@playwright/test';\nimport { price } from '../../src/price.js';\n\ntest('pays', async ({ request }) => {\n  expect(await (await request.get('http://127.0.0.1:${pay}/charge')).text()).toBe('paid');\n  expect(price(500)).toBe('large');\n});\n`,
  'tests/e2e/mail.spec.js': `import { expect, test } from '@playwright/test';\nimport { price } from '../../src/price.js';\n\ntest('mails', async ({ request }) => {\n  expect(await (await request.get('http://127.0.0.1:${mail}/send')).text()).toBe('sent');\n  expect(price(5)).toBe('small');\n});\n`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });
const measure = (args = []) => {
  const measured = run(binary, ['--', 'npx', 'playwright', 'test', ...args]);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  return JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const suite = measure();
  // The pay mock's data changes: only the test it served is affected.
  writeFileSync(resolve(root, 'tests/mocks/pay-api/answers.json'), '{ "status": "paid", "currency": "eur" }\n');
  const affected = run(binary, ['runs', suite, 'tests', 'affected', '--names']);
  assert.equal(affected.status, 0, affected.stderr);
  assert.deepEqual(affected.stdout.trim().split('\n'), ['pay.spec.js pays'], affected.stdout);
  const reasons = run(binary, ['runs', suite, 'tests', 'affected']).stdout;
  assert.match(reasons, /answers\.json.*a server that served this test loaded it/u, reasons);
  // Merging the suite with a run that did not measure it again is refused.
  const mailOnly = measure(['tests/e2e/mail.spec.js']);
  const refused = run(binary, ['merge', suite, mailOnly]);
  assert.notEqual(refused.status, 0, refused.stdout + refused.stderr);
  assert.match(refused.stderr, /pay\.spec\.js > pays/u, refused.stderr);
  // Measured again alone, it merges with the suite's run, which keeps the
  // other test and leaves the old record of this one out.
  const payOnly = measure(['tests/e2e/pay.spec.js']);
  const merged = run(binary, ['merge', suite, payOnly]);
  assert.equal(merged.status, 0, merged.stdout + merged.stderr);
  const summary = run(binary, ['runs', 'latest']).stdout;
  assert.match(summary, /merged across: tests\/mocks\/pay-api\/answers\.json \(the 1 test\(s\)/u, summary);
  assert.match(summary, /Total {7}2/u, summary);
  // What the mocks read cannot be recorded (SUPERCOV_TEST_FAULT): the run
  // is published without it.
  const unrecorded = spawnSync(binary, ['--', 'npx', 'playwright', 'test'], { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', SUPERCOV_TEST_FAULT: 'support-inputs' } });
  assert.equal(unrecorded.status, 0, unrecorded.stdout + unrecorded.stderr);
  console.log('[mock-reach] a changed mock affects only the tests it served, and measuring those alone merges into the suite\'s run');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'mock-reach-'))) rmSync(root, { recursive: true, force: true });
}
