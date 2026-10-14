// Browser tests of a Ruby server measure the server.
//
//   node scripts/ruby-server-playwright.mjs
//
// A Playwright suite whose `webServer` is a Rack app under Puma, installed
// with Bundler into the project: the project has no JavaScript source, and
// the run is the server's Ruby run, each request credited to the test that
// sent it -- by header from the request fixture, by cookie from a page -- a
// Ruby process a test starts credited to that test, and a webhook an
// unmeasured mock sends credited by time. The same app under WEBrick.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Supercov measures Ruby 3.3 and newer; the system Ruby may be older.
const ruby = process.env.SUPERCOV_RUBY ?? ['/opt/homebrew/opt/ruby/bin/ruby'].find((candidate) => existsSync(candidate)) ?? 'ruby';
const rubyBin = ruby.includes('/') ? dirname(ruby) : undefined;
const root = resolve(repository, 'target', `ruby-server-playwright-${process.pid}-${Date.now()}`);
const port = 48000 + Math.floor(Math.random() * 1500);
const stripe = port + 1;

if (process.platform === 'win32') {
  console.log('[ruby-server-playwright] skipped on Windows: the fixture starts its mock through env -u');
  process.exit(0);
}

const path = [rubyBin, process.env.PATH].filter(Boolean).join(':');
const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, PATH: path, NO_COLOR: '1', CI: '1', ...env } });
mkdirSync(root, { recursive: true });
const version = run(ruby, ['-e', 'print RUBY_VERSION']);
assert.equal(version.status, 0, version.stderr);
const [major, minor] = version.stdout.split('.').map(Number);
assert.ok(major * 100 + minor >= 303, `Ruby ${version.stdout} is older than the 3.3 Supercov measures; set SUPERCOV_RUBY`);

const files = {
  'package.json': '{ "name": "ruby-server", "private": true, "type": "module" }\n',
  Gemfile: 'source "https://rubygems.org"\ngem "sinatra"\ngem "puma"\ngem "rackup"\ngem "webrick"\n',
  'app/grades.rb': 'module Grades\n  def self.grade(score)\n    return "A" if score >= 90\n    return "pass" if score >= 50\n\n    "fail"\n  end\n\n  def self.unused\n    "never"\n  end\nend\n',
  'app/payments.rb': 'module Payments\n  def self.settle(event)\n    return "paid" if event.include?("charge.succeeded")\n\n    "ignored"\n  end\nend\n',
  'app/web.rb': `require "net/http"
require "sinatra/base"
require_relative "grades"
require_relative "payments"

class Web < Sinatra::Base
  set :order, "none"

  get("/ready") { "ready" }
  get("/grade") { Grades.grade(Integer(params["score"])) }
  get("/") do
    "<html><body><script>fetch('/grade?score=95').then((r) => r.text()).then((t) => { document.body.dataset.top = t; });</script></body></html>"
  end
  get("/checkout") do
    settings.order = "pending"
    Net::HTTP.post(URI("http://127.0.0.1:${stripe}/charge"), "")
    "started"
  end
  # Slow, so that a poll of the test's is running on another thread when
  # the webhook comes, and leaves while it runs: the webhook is no poll's.
  get("/state") do
    sleep 0.2
    settings.order
  end
  post("/webhook") do
    settings.order = Payments.settle(request.body.read)
    "ok"
  end
end
`,
  'config.ru': 'require_relative "app/web"\nrun Web\n',
  // Answers the charge, then calls the app's webhook by itself.
  'mocks/stripe.mjs': `import { createServer } from 'node:http';\n\ncreateServer((request, response) => {\n  if (request.url === '/charge') {\n    response.end('{}');\n    setTimeout(() => fetch('http://127.0.0.1:${port}/webhook', { method: 'POST', body: JSON.stringify({ type: 'charge.succeeded' }) }), 100);\n    return;\n  }\n  response.end('ready');\n}).listen(${stripe}, '127.0.0.1');\n`,
  'playwright.config.js': `const servers = {
  puma: 'bundle exec puma -b tcp://127.0.0.1:${port} -t 1:4',
  webrick: 'bundle exec rackup -s webrick -o 127.0.0.1 -p ${port}',
};
export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: [
    { command: servers[process.env.SERVER], url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false },
    // Started without Supercov, as an external mock binary would be.
    { command: 'env -u NODE_OPTIONS node mocks/stripe.mjs', url: 'http://127.0.0.1:${stripe}/', reuseExistingServer: false },
  ],
};
`,
  'tests/grades.spec.js': `import { spawnSync } from 'node:child_process';
import { expect, test } from '@playwright/test';

test('a high score is an A', async ({ request }) => {
  expect(await (await request.get('/grade?score=95')).text()).toBe('A');
});

test('a low score fails', async ({ request }) => {
  expect(await (await request.get('/grade?score=10')).text()).toBe('fail');
});

test('the page asks for the top grade', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('body')).toHaveAttribute('data-top', 'A');
});

test('a test that sends nothing', async () => {
  expect(1).toBe(1);
});

test('a script the test runs', async () => {
  const graded = spawnSync(${JSON.stringify(ruby)}, ['-e', 'require_relative "app/grades"; puts Grades.grade(70)'], { encoding: 'utf8' });
  expect(graded.stdout.trim()).toBe('pass');
});

test('a checkout is paid when the webhook arrives', async ({ request }) => {
  await request.get('/checkout');
  await expect.poll(async () => (await request.get('/state')).text()).toBe('paid');
});
`,
};

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  for (const step of [
    ['bundle', ['config', 'set', '--local', 'path', 'vendor/bundle']],
    ['bundle', ['install', '--quiet']],
  ]) {
    const done = run(...step);
    assert.equal(done.status, 0, `${step.flat().join(' ')}: ${done.stdout}${done.stderr}`);
  }
  for (const server of ['puma', 'webrick']) {
    const measured = run(binary, ['--', 'npx', 'playwright', 'test'], { SERVER: server });
    assert.equal(measured.status, 0, `${server}: ${measured.stdout}${measured.stderr}`);
    assert.match(measured.stderr, /Ruby server coverage: 6 test\(s\) across 3 source file\(s\)/u, measured.stderr);
    const id = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
    const page = (...query) => {
      const result = run(binary, ['runs', id, ...query]);
      assert.equal(result.status, 0, result.stdout + result.stderr);
      return result.stdout;
    };
    const lines = (name, file = 'grades') => page('test', name).match(new RegExp(`app/${file}\\.rb:\\d+`, 'gu')) ?? [];
    assert.deepEqual(lines('a high score is an A'), ['app/grades.rb:2', 'app/grades.rb:3'], server);
    assert.deepEqual(lines('a low score fails'), ['app/grades.rb:2', 'app/grades.rb:3', 'app/grades.rb:4', 'app/grades.rb:6'], server);
    assert.deepEqual(lines('the page asks for the top grade'), ['app/grades.rb:2', 'app/grades.rb:3'], server);
    assert.deepEqual(lines('a test that sends nothing'), [], server);
    // Its own process loads the file too: the module's lines ran under it.
    assert.ok(lines('a script the test runs').includes('app/grades.rb:4'), `${server}: ${lines('a script the test runs')}`);
    const paid = page('test', 'a checkout is paid when the webhook arrives');
    assert.match(paid, /app\/payments\.rb:3/u, `${server}: the webhook is not the checkout test's:\n${paid}`);
    assert.match(paid, /credited by time/u, paid);
    assert.doesNotMatch(page('test', 'a high score is an A'), /payments\.rb|credited by time/u);
  }
  console.log('[ruby-server-playwright] a Ruby server under browser tests is measured, each request credited to its test');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'ruby-server-playwright-'))) rmSync(root, { recursive: true, force: true });
}
