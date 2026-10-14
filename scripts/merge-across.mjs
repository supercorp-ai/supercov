// A run of new end-to-end tests is added to the suite's run before it when
// the tests brought helpers and runner settings with them, and is not when a
// setting that decides what a test does changed.
//
//   node scripts/merge-across.mjs
//
// A Playwright suite against a small server: measure it, then the batch a
// reviewer measures alone -- a helper that gains a function, a longer
// timeout, one new spec -- and merge with no run named. Then a header every
// request sends, which must leave the first run out; and a change to a helper
// function the first spec calls, which `tests affected` must report.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `merge-across-${process.pid}-${Date.now()}`);
const port = 41000 + Math.floor(Math.random() * 8000);

const files = {
  'package.json': '{ "name": "merge-across", "private": true, "type": "module" }\n',
  'src/grade.js': "export function grade(value) {\n  if (value > 10) return 'high';\n  if (value > 5) return 'mid';\n  return 'low';\n}\n",
  'server.mjs': `import { createServer } from 'node:http';\nimport { grade } from './src/grade.js';\n\ncreateServer((request, response) => {\n  const url = new URL(request.url, 'http://localhost');\n  response.writeHead(200, { 'content-type': 'application/json' });\n  response.end(JSON.stringify({ grade: grade(Number(url.searchParams.get('value'))), suite: request.headers['x-suite'] ?? null }));\n}).listen(${port}, '127.0.0.1');\n`,
  'playwright.config.js': `export default {\n  testDir: 'tests/e2e',\n  workers: 1,\n  reporter: 'line',\n  timeout: 30000,\n  use: { baseURL: 'http://127.0.0.1:${port}' },\n  webServer: { command: 'node server.mjs', url: 'http://127.0.0.1:${port}/?value=0', reuseExistingServer: false },\n};\n`,
  'tests/e2e/helpers.js': "export async function gradeOf(request, value) {\n  const response = await request.get(`/?value=${value}`);\n  return (await response.json()).grade;\n}\n",
  'tests/e2e/high.spec.js': "import { expect, test } from '@playwright/test';\nimport { gradeOf } from './helpers.js';\n\ntest('a large value is high', async ({ request }) => {\n  expect(await gradeOf(request, 11)).toBe('high');\n});\n",
};

function write(file, text) {
  mkdirSync(dirname(resolve(root, file)), { recursive: true });
  writeFileSync(resolve(root, file), text);
}

function edit(file, from, to) {
  const text = readFileSync(resolve(root, file), 'utf8');
  assert.equal(text.split(from).length, 2, `${from} in ${file}`);
  write(file, text.replace(from, to));
}

function supercov(...args) {
  const result = spawnSync(binary, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });
  assert.equal(result.error, undefined, `supercov did not start: ${result.error}`);
  return { ...result, printed: `${result.stdout}${result.stderr}` };
}

function measure(...specs) {
  const result = supercov('--', 'npx', 'playwright', 'test', ...specs);
  assert.equal(result.status, 0, `the suite failed under Supercov:\n${result.printed}`);
  return JSON.parse(supercov('runs', '--json').stdout).data.runs[0].id;
}

try {
  for (const [file, text] of Object.entries(files)) write(file, text);
  const baseline = measure();

  // The batch: a helper gains a function, the timeout grows, one new spec.
  edit('tests/e2e/helpers.js', '.grade;\n}\n', ".grade;\n}\n\nexport async function suiteOf(request) {\n  const response = await request.get('/?value=0');\n  return (await response.json()).suite;\n}\n");
  edit('playwright.config.js', 'timeout: 30000', 'timeout: 45000');
  write('tests/e2e/mid.spec.js', "import { expect, test } from '@playwright/test';\nimport { gradeOf, suiteOf } from './helpers.js';\n\ntest('a middling value is mid', async ({ request }) => {\n  expect(await gradeOf(request, 7)).toBe('mid');\n  expect(await suiteOf(request)).toBe(null);\n});\n");
  const batch = measure('tests/e2e/mid.spec.js');
  const merged = supercov('merge');
  assert.equal(merged.status, 0, merged.printed);
  assert.match(merged.printed, /merging 2 runs/u, merged.printed);
  assert.ok(merged.printed.includes(baseline) && merged.printed.includes(batch), merged.printed);
  const summary = supercov('runs', 'latest').stdout;
  assert.match(summary, /merged across: .*playwright\.config\.js \(only settings that schedule tests\)/u, summary);
  assert.match(summary, /tests\/e2e\/helpers\.js \(only added suiteOf\)/u, summary);
  const data = JSON.parse(supercov('runs', 'latest', '--json').stdout).data;
  assert.equal(data.tests, 2, JSON.stringify(data));
  const before = JSON.parse(supercov('runs', baseline, '--json').stdout).data;
  // Only the new spec takes the middle branch.
  assert.ok(data.coverage.branches.covered > before.coverage.branches.covered, `the new spec's branches count: ${JSON.stringify([before.coverage.branches, data.coverage.branches])}`);

  // A header every request sends decides what a test does.
  edit('playwright.config.js', "use: { baseURL:", "use: { extraHTTPHeaders: { 'x-suite': 'e2e' }, baseURL:");
  edit('tests/e2e/mid.spec.js', 'toBe(null)', "toBe('e2e')");
  measure('tests/e2e/mid.spec.js');
  const refused = supercov('merge');
  assert.match(refused.printed, new RegExp(`left out ${baseline} .*playwright\\.config\\.js: use changed`, 'u'), refused.printed);

  // A helper function the first spec calls: that spec is affected.
  edit('tests/e2e/helpers.js', 'return (await response.json()).grade;', 'return String((await response.json()).grade);');
  const affected = JSON.parse(supercov('runs', baseline, 'tests', 'affected', '--json').stdout).data;
  const reasons = JSON.stringify(affected.affected);
  assert.match(reasons, /tests\/e2e\/helpers\.js changed: .*gradeOf.*\(this test's file loads it\)/u, reasons);
  console.log(`[merge-across] new specs merge across an added helper and a longer timeout; a header and a changed helper are counted`);
} finally {
  if (root.startsWith(resolve(repository, 'target', 'merge-across-'))) rmSync(root, { recursive: true, force: true });
}
