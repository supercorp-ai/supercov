// A context that outlives its tests runs one copy of each test's page script.
//
//   node scripts/context-script-copies.mjs
//
// A worker-scoped context is registered again by every test, and each
// registration added the script that tells a new document which test it
// belongs to. They accumulated: on a long suite a document ran some fifty
// copies, each sent to every new page and frame. The previous test's copy is
// now removed once the next one is in place, so a document runs one copy,
// two while a removal is still on its way. Each test's document still knows
// its own test, and each test is still credited with what its page ran.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `context-script-copies-${process.pid}-${Date.now()}`);
const port = await freePort();
const tests = 10;

const files = {
  'package.json': '{ "name": "context-script-copies", "private": true, "type": "module" }\n',
  'server.mjs': `import { createServer } from 'node:http';\nimport { readFileSync } from 'node:fs';\n\ncreateServer((request, response) => {\n  const path = new URL(request.url, 'http://x').pathname;\n  const file = path === '/' ? 'index.html' : path.slice(1);\n  try {\n    const body = readFileSync(new URL(\`./public/\${file}\`, import.meta.url));\n    response.writeHead(200, { 'content-type': file.endsWith('.js') ? 'text/javascript' : 'text/html' });\n    response.end(body);\n  } catch {\n    response.writeHead(404);\n    response.end();\n  }\n}).listen(${port});\n`,
  // The first script of the document reads the count before anything but
  // the init scripts ran: Supercov's own activation tells a document its
  // test again at each action, after it loads.
  'public/index.html': '<!doctype html><head><script>window.__TOLD_AT_LOAD__ = window.__TOLD__;</script></head><button id="press">press</button><p id="out"></p><script type="module" src="/app.js"></script>\n',
  'public/app.js': "export function press() {\n  document.getElementById('out').textContent = 'pressed';\n}\n\ndocument.getElementById('press').addEventListener('click', press);\n",
  'playwright.config.js': `export default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${port}', browserName: 'chromium' },\n  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/', reuseExistingServer: false },\n};\n`,
  // One context for the worker. Before any test, a script of the suite's own
  // counts each time a document is told its test: it runs ahead of every
  // script registered after it.
  'tests/fixtures.js': `import { test as base } from '@playwright/test';
import { appendFileSync } from 'node:fs';
import { freePort } from './free-ports.mjs';

export const test = base.extend({
  shared: [async ({ browser }, use) => {
    const context = await browser.newContext({ baseURL: 'http://127.0.0.1:${port}' });
    await context.addInitScript(() => {
      let attempt;
      let told = 0;
      Object.defineProperty(globalThis, '__SUPERCOV_ATTEMPT__', {
        configurable: true,
        get() { return attempt; },
        set(value) { told += 1; attempt = value; },
      });
      Object.defineProperty(globalThis, '__TOLD__', { configurable: true, get() { return told; } });
    });
    await use(context);
    await context.close();
  }, { scope: 'worker' }],
  seen: async ({}, use) => {
    await use((entry) => appendFileSync(${JSON.stringify(resolve(root, 'seen.jsonl'))}, JSON.stringify(entry) + '\\n'));
  },
});
export { expect } from '@playwright/test';
`,
  'tests/copies.spec.js': `import { test, expect } from './fixtures.js';

for (let index = 1; index <= ${tests}; index += 1) {
  test(\`test \${index}\`, async ({ shared, seen }) => {
    const page = await shared.newPage();
    await page.goto('/');
    await page.click('#press');
    await expect(page.locator('#out')).toHaveText('pressed');
    const { told, attempt } = await page.evaluate(() => ({ told: globalThis.__TOLD_AT_LOAD__, attempt: globalThis.__SUPERCOV_ATTEMPT__?.attemptId }));
    seen({ index, told, attempt });
    await page.close();
  });
}
`,
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_SOURCE_ROOTS: 'public' });
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const seen = readFileSync(resolve(root, 'seen.jsonl'), 'utf8').trim().split('\n').map((line) => JSON.parse(line));
  assert.equal(seen.length, tests, JSON.stringify(seen));
  // Each document knew its own test: the newest copy ran.
  assert.equal(new Set(seen.map((entry) => entry.attempt)).size, tests, JSON.stringify(seen));
  assert.ok(seen.every((entry) => typeof entry.attempt === 'string'), JSON.stringify(seen));
  // And ran one copy, or two while the last removal was on its way, where
  // the tenth test's document ran ten. The context is made in the first
  // test, after that test's registration, so its document ran none and was
  // told its test by the first action, as the attempt above shows.
  for (const entry of seen)
    assert.ok(entry.index === 1 ? entry.told <= 1 : entry.told >= 1 && entry.told <= 2, JSON.stringify(seen));
  // Each test is credited with its own click.
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const answer = run(binary, ['runs', runId, 'line', 'public/app.js:2', '--json']);
  assert.equal(answer.status, 0, answer.stdout + answer.stderr);
  const credited = JSON.parse(answer.stdout).data.tests
    .filter((test) => test.provenance.kind !== 'background')
    .map((test) => test.name.split(' > ').at(-1))
    .sort((left, right) => Number(left.split(' ')[1]) - Number(right.split(' ')[1]));
  assert.deepEqual(credited, Array.from({ length: tests }, (_, index) => `test ${index + 1}`));
  console.log(`[context-script-copies] ${tests} tests on one worker-scoped context: each document ran ${[...new Set(seen.map((entry) => entry.told))].join(' or ')} copy of its test's script, knew its own test, and each test is credited with its own click`);
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'context-script-copies-'))) rmSync(root, { recursive: true, force: true });
}
