// A Next.js webhook route that does its work in `after()`, and a Playwright
// test that waits for that work's effect: a strong pass kills the mutants of
// the work.
//
//   node scripts/next-after-webhook.mjs
//
// A mock payment provider, a process of its own, sends the webhook when the
// test asks it to, and the webhook names no test (as a provider's never
// does). The route answers at once and reports the sale to a mock affiliate
// service in `after()`; the test polls that service. The test's own first
// request to the route (one that names it) is where Next.js loads the
// route's module.
//
// Before, the handler wrapped as that module loaded took the loading
// request's attempt as its owner: every later request that named no test ran
// as that attempt's, and in a strong pass with its mutant -- that of the
// round's first run of the test, none. So a mutant that stops the report
// lived, though the test fails with it. And the route was given a web
// `Request`, which carries no lifetime, so its `after()` work stayed the
// request's. Now the webhook is a request no test named, and the work after
// its answer is the server's own: the mutant runs alone, with the server
// started with it switched on, and is killed.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePorts } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the app finds its Next.js and the suite its Playwright.
const root = resolve(repository, 'target', `next-after-webhook-${process.pid}-${Date.now()}`);
const [app, mock] = await freePorts(2);

const files = {
  'package.json': '{ "name": "next-after-webhook", "private": true }\n',
  'next.config.mjs': 'export default {};\n',
  'app/layout.jsx': 'export default function RootLayout({ children }) {\n  return (\n    <html lang="en">\n      <body>{children}</body>\n    </html>\n  );\n}\n',
  'app/page.jsx': 'export default function Home() {\n  return <h1>Sales</h1>;\n}\n',
  // The webhook answers at once and reports the sale after its answer.
  'app/api/webhooks/sale/route.js': "import { after } from 'next/server';\nimport { recordSale } from '../../../../lib/sales.js';\n\nexport async function POST(request) {\n  const sale = await request.json();\n  after(() => recordSale(sale));\n  return Response.json({ received: true });\n}\n",
  'lib/sales.js': "import { report } from './affiliates.js';\n\nexport async function recordSale(sale) {\n  if (sale.amount > 0) await report(sale);\n}\n",
  'lib/affiliates.js': "export async function report(sale) {\n  await fetch(`${process.env.AFFILIATES_URL}/sales`, { method: 'POST', body: JSON.stringify(sale) });\n}\n",
  // The provider sends the webhook by itself; the affiliates keep the sales.
  'mocks/provider.mjs': `import { createServer } from 'node:http';\n\nconst sales = [];\ncreateServer(async (request, response) => {\n  let body = '';\n  for await (const chunk of request) body += chunk;\n  const url = new URL(request.url, 'http://local');\n  if (request.method === 'POST' && url.pathname === '/deliver') {\n    const delivered = await fetch('http://127.0.0.1:${app}/api/webhooks/sale', { method: 'POST', body });\n    return response.end(JSON.stringify({ status: delivered.status }));\n  }\n  if (request.method === 'POST' && url.pathname === '/sales') {\n    sales.push(JSON.parse(body));\n    return response.end('{}');\n  }\n  if (url.pathname === '/sales') return response.end(JSON.stringify(sales.filter((sale) => sale.id === url.searchParams.get('id'))));\n  response.end('ready');\n}).listen(${mock}, '127.0.0.1');\n`,
  // A later round serves the build its first one made, as a suite that
  // builds once does.
  'playwright.config.js': `import { existsSync } from 'node:fs';\n\nexport default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: 'http://127.0.0.1:${app}' },\n  webServer: [\n    {\n      command: \`\${process.env.SUPERCOV_STRONG_RESTART && existsSync('.next/BUILD_ID') ? '' : 'npx next build --webpack . && '}npx next start . -p ${app}\`,\n      url: 'http://127.0.0.1:${app}/',\n      reuseExistingServer: false,\n      timeout: 180000,\n      env: { AFFILIATES_URL: 'http://127.0.0.1:${mock}' },\n    },\n    { command: 'node mocks/provider.mjs', url: 'http://127.0.0.1:${mock}/', reuseExistingServer: false },\n  ],\n};\n`,
  'tests/sales.spec.js': `import { expect, test } from '@playwright/test';\n\nconst provider = 'http://127.0.0.1:${mock}';\n\ntest('a sale the provider reports reaches the affiliates', async ({ request }) => {\n  // The application's own request names this test; the route loads here.\n  expect((await request.post('/api/webhooks/sale', { data: { id: 'nothing', amount: 0 } })).status()).toBe(200);\n  const id = \`sale-\${Date.now()}\`;\n  const sent = await fetch(\`\${provider}/deliver\`, { method: 'POST', body: JSON.stringify({ id, amount: 5 }) });\n  expect(await sent.json()).toEqual({ status: 200 });\n  await expect.poll(async () => (await fetch(\`\${provider}/sales?id=\${id}\`)).json(), { timeout: 3000 }).toEqual([{ id, amount: 5 }]);\n});\n`,
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env }, maxBuffer: 64 * 1024 * 1024 });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const roots = { SUPERCOV_SOURCE_ROOTS: 'app,lib' };
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], roots);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const runId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  // The work after the webhook's answer is the server's own.
  const line = run(binary, ['runs', runId, 'line', 'lib/sales.js:4']);
  assert.match(line.stdout, /Background \/ unattributed/u, line.stdout);
  const strong = run(binary, ['runs', runId, 'mutants', 'test', '--file', 'lib/sales.js'], roots);
  assert.equal(strong.status, 0, `${strong.stdout}${strong.stderr}`);
  const mutants = JSON.parse(run(binary, ['runs', runId, 'mutants', 'lib/sales.js', '--json', '--limit', '0']).stdout).data.mutants;
  const context = `${JSON.stringify(mutants, null, 2)}\n${strong.stdout}${strong.stderr}`;
  // No report: the test waits for one, and fails.
  for (const [original, replacement] of [['sale.amount > 0', 'false'], ['sale.amount > 0', 'sale.amount <= 0']]) {
    const mutant = mutants.find((entry) => entry.original === original && entry.replacement === replacement);
    assert.equal(mutant?.strong?.status, 'killed', `${original} -> ${replacement}\n${context}`);
    // A mutant compiling to the same code as another takes that one's verdict; the reason is then the twin's.
    const twinId = mutant.strong.reason.match(/^same-code-as:(.+)$/u)?.[1];
    const decided = twinId ? mutants.find((entry) => entry.id === twinId) : mutant;
    assert.equal(decided?.strong?.status, 'killed', `${original} -> ${replacement} (twin ${twinId})\n${context}`);
    assert.match(decided.strong.reason, /ran alone, with the server started with it switched on: its code ran in server work no request was waiting for/u, context);
  }
  console.log('[next-after-webhook] a Next.js webhook naming no test, whose route works in after(), has its mutants killed: run alone, with the server started with them');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'next-after-webhook-'))) rmSync(root, { recursive: true, force: true });
}
