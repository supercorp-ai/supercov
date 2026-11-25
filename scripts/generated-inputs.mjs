// Files the measured command writes are not inputs of the run.
//
//   node scripts/generated-inputs.mjs
//
// A suite measured in parts on several machines: each machine's harness
// writes its own settings file (git-ignored, different ports and keys on
// each), which the suite's server reads. Each part's command writes it into
// the copy it runs in. The parts merge: the file is what the command made,
// not something a later checkout could change under the run. A file the
// server reads that the project tracks and the command leaves alone is still
// an input: changed, it affects the tests the server served. And the merged
// run answers with its parts gone, as a baseline cached alone does.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePort } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `generated-inputs-${process.pid}-${Date.now()}`);
const port = await freePort();

const files = {
  '.gitignore': 'tests/stack/.env.local\n',
  'package.json': '{ "name": "generated-inputs", "private": true, "type": "module" }\n',
  'src/price.js': "export function price(amount) {\n  return amount > 100 ? 'large' : 'small';\n}\n",
  'tests/mocks/answers.json': '{ "status": "paid" }\n',
  // Reads the machine's settings and the tracked answers on each request.
  'tests/mocks/api.mjs': `import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
const read = (file) => readFileSync(new URL(file, import.meta.url), 'utf8');
createServer((request, response) => {
  if (request.url === '/ready') return response.end('ready');
  const key = read('../stack/.env.local').trim();
  response.end(key.startsWith('KEY=') ? JSON.parse(read('./answers.json')).status : 'no key');
}).listen(Number(process.env.PORT));
`,
  'playwright.config.js': `export default {
  testDir: 'tests/e2e',
  workers: 1,
  reporter: 'line',
  webServer: { command: 'node tests/mocks/api.mjs', env: { PORT: '${port}' }, url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false },
};
`,
  'tests/e2e/large.spec.js': `import { expect, test } from '@playwright/test';\nimport { price } from '../../src/price.js';\n\ntest('large', async ({ request }) => {\n  expect(await (await request.get('http://127.0.0.1:${port}/charge')).text()).toBe('paid');\n  expect(price(500)).toBe('large');\n});\n`,
  'tests/e2e/small.spec.js': `import { expect, test } from '@playwright/test';\nimport { price } from '../../src/price.js';\n\ntest('small', async ({ request }) => {\n  expect(await (await request.get('http://127.0.0.1:${port}/charge')).text()).toBe('paid');\n  expect(price(5)).toBe('small');\n});\n`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });
// One machine's part: the stack booted beside the checkout writes its
// settings there, and the command copies them into the copy it runs in.
const part = (machine, spec) => {
  mkdirSync(resolve(root, 'tests/stack'), { recursive: true });
  writeFileSync(resolve(root, 'tests/stack/.env.local'), `KEY=${machine}-${'k'.repeat(machine)}\nPORT=${54000 + machine}\n`);
  const measured = run(binary, ['--', 'sh', '-c', `mkdir -p tests/stack && cp "${resolve(root, 'tests/stack/.env.local')}" tests/stack/ && npx playwright test ${spec}`]);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  return JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  for (const args of [['init', '-q'], ['add', '.'], ['-c', 'user.name=t', '-c', 'user.email=t@t', 'commit', '-qm', 'suite']]) {
    assert.equal(run('git', args).status, 0);
  }
  const first = part(1, 'tests/e2e/large.spec.js');
  const second = part(2, 'tests/e2e/small.spec.js');
  // Each machine's settings differ, and the parts merge.
  const merged = run(binary, ['merge', first, second]);
  assert.equal(merged.status, 0, merged.stdout + merged.stderr);
  const summary = run(binary, ['runs', 'latest']).stdout;
  assert.match(summary, /Total {7}2/u, summary);
  assert.doesNotMatch(summary, /merged across/u, summary);
  const whole = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  // The merged run stands alone: kept by itself, as a baseline is cached,
  // it answers without its parts.
  for (const id of [first, second]) rmSync(resolve(root, '.supercov/runs', id), { recursive: true });
  // Nor does a settings file another machine writes affect any test.
  writeFileSync(resolve(root, 'tests/stack/.env.local'), 'KEY=3\n');
  const unaffected = run(binary, ['runs', whole, 'tests', 'affected', '--names']);
  assert.equal(unaffected.status, 0, unaffected.stderr);
  assert.equal(unaffected.stdout.trim(), '', unaffected.stdout);
  // The tracked answers the server read are an input still.
  writeFileSync(resolve(root, 'tests/mocks/answers.json'), '{ "status": "paid", "currency": "eur" }\n');
  const affected = run(binary, ['runs', whole, 'tests', 'affected', '--names']);
  assert.equal(affected.status, 0, affected.stderr);
  assert.deepEqual(affected.stdout.trim().split('\n').sort(), ['large.spec.js large', 'small.spec.js small'], affected.stdout);
  console.log('[generated-inputs] parts whose commands wrote different settings files merge into a run that answers without them, and a tracked file the server read is an input still');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'generated-inputs-'))) rmSync(root, { recursive: true, force: true });
}
