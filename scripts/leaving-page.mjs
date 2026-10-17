// What a page ran before it navigated away is the test's.
//
//   node scripts/leaving-page.mjs
//
// A click handler that ends by setting `location.href` leaves the document
// it ran in. A page stores its evidence in localStorage as it leaves, and the
// next document restores it, but only on the same origin: an admin's
// masquerade link that sent the page from 127.0.0.1 to localhost lost every
// hit the handler made, though the test passes only if it ran. Both a page
// that stays on its origin and one that leaves it must keep the handler. So
// must an iframe the page removes after the test clicked in it: it gets no
// `beforeunload`, and what it ran went with it.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePorts } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `leaving-page-${process.pid}-${Date.now()}`);
const [port] = await freePorts(1);

const files = {
  'package.json': '{ "name": "leaving-page", "private": true, "type": "module" }\n',
  'server.mjs': `import { createServer } from 'node:http';\nimport { readFileSync } from 'node:fs';\n\ncreateServer((request, response) => {\n  const path = new URL(request.url, 'http://x').pathname;\n  const file = path === '/' ? 'index.html' : path.slice(1);\n  try {\n    const body = readFileSync(new URL(\`./public/\${file}\`, import.meta.url));\n    response.writeHead(200, { 'content-type': file.endsWith('.js') ? 'text/javascript' : 'text/html' });\n    response.end(body);\n  } catch {\n    response.writeHead(404);\n    response.end();\n  }\n}).listen(${port});\n`,
  'public/index.html': '<!doctype html><button id="same">same</button><button id="away">away</button><script type="module" src="/app.js"></script>\n',
  'public/done.html': '<!doctype html><p>done</p><script type="module" src="/app.js"></script>\n',
  'public/app.js': "export function leaveFor(target) {\n  const marker = target.length;\n  if (marker > 0) {\n    window.location.href = target;\n  }\n}\n\nexport function leaveForAnotherOrigin(target) {\n  const marker = target.length;\n  if (marker > 0) {\n    window.location.href = target;\n    document.title = 'leaving';\n  }\n}\n\ndocument.getElementById('same')?.addEventListener('click', () => leaveFor('/done.html'));\ndocument.getElementById('away')?.addEventListener('click', () => leaveForAnotherOrigin(location.href.replace('127.0.0.1', 'localhost') + 'done.html'));\n",
  // A native dialog: Playwright's `Dialog` is answered as it is without
  // Supercov. Its `type()` returned a promise, and the dialog could not be
  // answered at all.
  'public/ask.html': '<!doctype html><button id="ask">ask</button><p id="answer"></p><script type="module" src="/ask.js"></script>\n',
  'public/ask.js': "export function ask() {\n  const answer = window.prompt('Copy this link', 'https://example.test/link');\n  document.getElementById('answer').textContent = answer ?? 'none';\n}\ndocument.getElementById('ask')?.addEventListener('click', ask);\n",
  'public/frame.html': '<!doctype html><iframe title="rating" src="/inner.html"></iframe><button id="drop">drop</button><p id="left"></p><script>document.getElementById("drop").addEventListener("click", () => { document.querySelector("iframe").remove(); document.getElementById("left").textContent = "dropped"; });</script>\n',
  'public/inner.html': '<!doctype html><button id="rate">rate</button><p id="stars"></p><script type="module" src="/inner.js"></script>\n',
  'public/inner.js': "export function rate() {\n  const stars = 5;\n  document.getElementById('stars').textContent = String(stars);\n}\ndocument.getElementById('rate')?.addEventListener('click', rate);\n",
  'tests/frame.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('an iframe is rated, then removed', async ({ page }) => {\n  await page.goto('/frame.html');\n  const frame = page.frameLocator('iframe');\n  await frame.locator('#rate').click();\n  await expect(frame.locator('#stars')).toHaveText('5');\n  await page.click('#drop');\n  await expect(page.locator('#left')).toHaveText('dropped');\n});\n",
  'tests/dialog.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a prompt is read and answered', async ({ page }) => {\n  await page.goto('/ask.html');\n  const dialogs = page.waitForEvent('dialog');\n  const clicked = page.click('#ask');\n  const dialog = await dialogs;\n  expect(dialog.type()).toBe('prompt');\n  expect(dialog.message()).toBe('Copy this link');\n  expect(dialog.defaultValue()).toBe('https://example.test/link');\n  await dialog.accept('answered');\n  await clicked;\n  await expect(page.locator('#answer')).toHaveText('answered');\n});\n",
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
  // What a handler runs after the assignment that starts the navigation
  // (line 12) runs after `beforeunload`, and a page leaving for another
  // origin is gone before anything it sends then reliably arrives: it is not
  // asserted, and the docs say so.
  for (const [title, file, line] of [['leaves for the same origin', 'app.js', 4], ['leaves for another origin', 'app.js', 11], ['a prompt is read and answered', 'ask.js', 3], ['an iframe is rated, then removed', 'inner.js', 2]]) {
    const covered = run(binary, ['runs', runId, 'line', `public/${file}:${line}`]);
    assert.equal(covered.status, 0, covered.stdout + covered.stderr);
    assert.match(covered.stdout, new RegExp(title, 'u'), `${title}: what ran before the document left is not the test's\n${covered.stdout}`);
  }
  console.log('[leaving-page] a handler that sends the page away, to its own origin or another, stays the test\'s, as does what a removed iframe ran, and a native dialog is answered as without Supercov');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'leaving-page-'))) rmSync(root, { recursive: true, force: true });
}
