// A page shared by several tests credits each with only what it ran.
//
//   node scripts/shared-page.mjs
//
// A serial describe opens one page in `beforeAll` and its tests take turns on
// it. The page's document kept every hit since it loaded, so each test was
// credited with what the tests before it ran, and with the `beforeAll`'s own
// work. Each test is credited with its own clicks; what ran before the first
// test is no test's, and still counts in the run.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `shared-page-${process.pid}-${Date.now()}`);
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
  'package.json': '{ "name": "shared-page", "private": true, "type": "module" }\n',
  'server.mjs': `import { createServer } from 'node:http';\nimport { readFileSync } from 'node:fs';\n\ncreateServer((request, response) => {\n  const path = new URL(request.url, 'http://x').pathname;\n  const file = path === '/' ? 'index.html' : path.slice(1);\n  try {\n    const body = readFileSync(new URL(\`./public/\${file}\`, import.meta.url));\n    response.writeHead(200, { 'content-type': file.endsWith('.js') ? 'text/javascript' : 'text/html' });\n    response.end(body);\n  } catch {\n    response.writeHead(404);\n    response.end();\n  }\n}).listen(${port});\n`,
  'public/index.html': '<!doctype html><button id="setup">setup</button><button id="first">first</button><button id="second">second</button><p id="out"></p><script type="module" src="/app.js"></script>\n',
  'public/app.js': "export function setUp() {\n  document.getElementById('out').textContent = 'ready';\n}\n\nexport function first() {\n  document.getElementById('out').textContent = 'first';\n}\n\nexport function second() {\n  document.getElementById('out').textContent = 'second';\n}\n\ndocument.getElementById('setup').addEventListener('click', setUp);\ndocument.getElementById('first').addEventListener('click', first);\ndocument.getElementById('second').addEventListener('click', second);\n",
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${port}', browserName: 'chromium' },\n  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },\n};\n`,
  'tests/shared.spec.js': `import { expect, test } from '@playwright/test';

test.describe.serial('one page', () => {
  let page;
  test.beforeAll(async ({ browser }) => {
    page = await browser.newPage({ baseURL: 'http://127.0.0.1:${port}' });
    await page.goto('/');
    await page.click('#setup');
    await expect(page.locator('#out')).toHaveText('ready');
  });
  test.afterAll(async () => {
    await page.close();
  });

  test('clicks first', async () => {
    await page.click('#first');
    await expect(page.locator('#out')).toHaveText('first');
  });

  test('clicks second', async () => {
    await page.click('#second');
    await expect(page.locator('#out')).toHaveText('second');
  });
});
`,
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const credited = (runId, line) => {
  const answer = run(binary, ['runs', runId, 'line', `public/app.js:${line}`, '--json']);
  assert.equal(answer.status, 0, answer.stdout + answer.stderr);
  const { data } = JSON.parse(answer.stdout);
  assert.equal(data.covered, true, `public/app.js:${line} is not covered\n${answer.stdout}`);
  return data.tests.filter((test) => test.provenance.kind !== 'background').map((test) => test.name.split(' > ').at(-1));
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_SOURCE_ROOTS: 'public' });
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  assert.deepEqual(credited(runId, 2), [], 'the beforeAll\'s click is no test\'s');
  assert.deepEqual(credited(runId, 6), ['clicks first']);
  assert.deepEqual(credited(runId, 10), ['clicks second']);
  console.log('[shared-page] tests sharing one page are each credited with their own clicks, and the beforeAll\'s work stays in the run as no test\'s');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'shared-page-'))) rmSync(root, { recursive: true, force: true });
}
