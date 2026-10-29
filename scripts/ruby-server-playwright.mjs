// Browser tests of a Ruby server measure the server.
//
//   node scripts/ruby-server-playwright.mjs
//
// A Playwright suite whose `webServer` is a Rack app under Puma, installed
// with Bundler into the project: the project has no JavaScript source, and
// the run is the server's Ruby run, each request credited to the test that
// sent it -- by header from the request fixture, by cookie from a page -- a
// Ruby process a test starts credited to that test, and a webhook an
// unmeasured mock sends credited by time. The same app under WEBrick. A
// Python server beside it publishes a run of its own; a failing test, a test
// that reaches no server and a filter that matches none each say what they
// published.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePorts } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Supercov measures Ruby 3.3 and newer; the system Ruby may be older.
const ruby = process.env.SUPERCOV_RUBY ?? ['/opt/homebrew/opt/ruby/bin/ruby'].find((candidate) => existsSync(candidate)) ?? 'ruby';
const rubyBin = ruby.includes('/') ? dirname(ruby) : undefined;
const root = resolve(repository, 'target', `ruby-server-playwright-${process.pid}-${Date.now()}`);
const [port, stripe, ping] = await freePorts(3);
const python = process.env.SUPERCOV_PYTHON ?? 'python3';

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
  Gemfile: 'source "https://rubygems.org"\ngem "sinatra"\ngem "puma"\ngem "rackup"\ngem "webrick"\ngem "minitest"\ngem "bootsnap"\n',
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
  // A server the suite runs in its own process, as Capybara runs Puma for a
  // Rails system test: its requests name no test, and each is the test's
  // that was running.
  'test/live_test.rb': "# As a Rails app's config/boot.rb does: Bootsnap caches compiled files.\nrequire \"bootsnap/setup\"\nrequire \"minitest/autorun\"\nrequire \"net/http\"\nrequire \"webrick\"\nrequire_relative \"../app/grades\"\n\n# A server the suite runs in its own process, as Capybara runs Puma.\nSERVER = WEBrick::HTTPServer.new(BindAddress: \"127.0.0.1\", Port: 0, Logger: WEBrick::Log.new(File::NULL), AccessLog: [])\nSERVER.mount_proc(\"/\") { |request, response| response.body = Grades.grade(Integer(request.query[\"score\"])) }\nThread.new { SERVER.start }\n\nclass LiveTest < Minitest::Test\n  def get(score)\n    Net::HTTP.get(URI(\"http://127.0.0.1:#{SERVER.config[:Port]}/?score=#{score}\"))\n  end\n\n  def test_a_low_score\n    assert_equal \"fail\", get(10)\n  end\n\n  def test_a_high_score\n    assert_equal \"A\", get(95)\n  end\nend\n",
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

test('a test skipped as it runs', async () => {
  test.skip(true, 'later');
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
    assert.match(measured.stderr, /Ruby server coverage: 7 test\(s\) across 3 source file\(s\)/u, measured.stderr);
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
    // A test that skipped itself is in the run, skipped.
    assert.match(page(), /Skipped\s+1\b/u, server);
    // A browser runner's test reaches the app over HTTP: whether its file
    // imports measured source is left unsaid.
    const unlinked = page('tests', 'without-evidence');
    assert.match(unlinked, /a test that sends nothing/u, unlinked);
    assert.doesNotMatch(unlinked, /; its file imports no measured source/u, unlinked);
    // Its own process loads the file too: the module's lines ran under it.
    assert.ok(lines('a script the test runs').includes('app/grades.rb:4'), `${server}: ${lines('a script the test runs')}`);
    const paid = page('test', 'a checkout is paid when the webhook arrives');
    assert.match(paid, /app\/payments\.rb:3/u, `${server}: the webhook is not the checkout test's:\n${paid}`);
    assert.match(paid, /credited by time/u, paid);
    assert.doesNotMatch(page('test', 'a high score is an A'), /payments\.rb|credited by time/u);
  }
  const live = run(binary, ['--', 'bundle', 'exec', 'ruby', 'test/live_test.rb'], { BOOTSNAP_CACHE_DIR: resolve(root, 'tmp/cache') });
  assert.equal(live.status, 0, live.stdout + live.stderr);
  const liveId = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
  const low = run(binary, ['runs', liveId, 'test', 'LiveTest#test_a_low_score']).stdout;
  assert.deepEqual(low.match(/app\/grades\.rb:\d+/gu), ['app/grades.rb:2', 'app/grades.rb:3', 'app/grades.rb:4', 'app/grades.rb:6'], low);
  assert.match(low, /credited by time/u, low);
  // Branches come from probes, which Bootsnap's compile cache skipped.
  assert.match(low, /[1-9][0-9]* decisions/u, `the test has lines and no decisions: Bootsnap's cache ran the file without probes\n${low}`);

  // A Python server beside the Ruby one, still no JavaScript source: the
  // first server's run is the run, the other's is published beside it.
  for (const [file, text] of Object.entries({
    'pinger/__init__.py': '',
    'pinger/answer.py': 'def answer(path):\n    if path == "/ping":\n        return b"pong"\n    return b"ready"\n',
    'pinger/serve.py': 'import sys\nfrom http.server import BaseHTTPRequestHandler, ThreadingHTTPServer\n\nfrom pinger.answer import answer\n\n\nclass Handler(BaseHTTPRequestHandler):\n    def do_GET(self):\n        data = answer(self.path)\n        self.send_response(200)\n        self.send_header("content-length", str(len(data)))\n        self.end_headers()\n        self.wfile.write(data)\n\n    def log_message(self, *args):\n        pass\n\n\nThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()\n',
    'both.config.js': `export default {
  testDir: 'both',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: [
    { command: 'bundle exec puma -b tcp://127.0.0.1:${port} -t 1:4', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false },
    { command: '${python} -m pinger.serve ${ping}', url: 'http://127.0.0.1:${ping}/ready', reuseExistingServer: false },
  ],
};
`,
    'both/both.spec.js': `import { expect, test } from '@playwright/test';

test('both servers answer', async ({ page, request }) => {
  expect(await (await request.get('/grade?score=95')).text()).toBe('A');
  // Another port of the same host: the page's cookie names the test there.
  await page.goto('http://127.0.0.1:${ping}/ping');
  await expect(page.locator('body')).toHaveText('pong');
});

test('a grade that is not given', async ({ request }) => {
  expect(await (await request.get('/grade?score=95')).text()).toBe('B');
});

test('a test that reaches no server', async () => {
  expect(1).toBe(1);
});
`,
  })) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const both = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'both servers answer']);
  assert.equal(both.status, 0, both.stdout + both.stderr);
  const ids = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs.map((entry) => entry.id);
  const beside = both.stderr.match(/the (?:Python|Ruby) server's coverage is run (run_[0-9a-f]{16})/u)?.[1];
  assert.ok(beside, both.stderr);
  assert.equal(ids[1], beside, `the first server's run is the latest, the other's beside it: ${ids}`);
  const bothId = ids[0];
  // Each server's run names the other: the latest names the run beside it,
  // and that run names the latest.
  const companionsOf = (id) => JSON.parse(run(binary, ['runs', id, '--json']).stdout).data.companions ?? [];
  assert.deepEqual(companionsOf(bothId).map((companion) => [companion.id, companion.role]), [[beside, 'server']]);
  assert.deepEqual(companionsOf(beside).map((companion) => [companion.id, companion.role]), [[bothId, 'server']]);
  assert.match(run(binary, ['runs', bothId]).stdout, new RegExp(`(?:Python|Ruby) server: run ${beside}`, 'u'));
  // Failures injected (SUPERCOV_TEST_FAULT): the copy taken by another run
  // after the servers were prepared for it, which prepares them again; a
  // server's run that cannot be published, which fails the run; and a failing
  // test whose servers publish nothing.
  const moved = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'both servers answer'], { SUPERCOV_TEST_FAULT: 'copy-moved' });
  assert.equal(moved.status, 0, moved.stdout + moved.stderr);
  assert.match(moved.stderr, /test fault at copy-moved/u, moved.stderr);
  assert.match(moved.stderr, /Python server coverage: 1 test/u, moved.stderr);
  assert.match(moved.stderr, /Ruby server coverage: 1 test/u, moved.stderr);
  const unpublishable = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'both servers answer'], { SUPERCOV_TEST_FAULT: 'server-publish' });
  assert.notEqual(unpublishable.status, 0, unpublishable.stdout + unpublishable.stderr);
  assert.match(unpublishable.stderr, /test fault at server-publish/u, unpublishable.stderr);
  assert.match(unpublishable.stderr, /server's run could not be published: injected fault at server-publish/u, unpublishable.stderr);
  const unpublished = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'not given'], { SUPERCOV_TEST_FAULT: 'server-unpublished' });
  assert.notEqual(unpublished.status, 0, unpublished.stdout + unpublished.stderr);
  assert.match(unpublished.stderr, /test fault at server-unpublished/u, unpublished.stderr);
  assert.match(unpublished.stderr, /no run was published/u, unpublished.stderr);
  // Each step after the servers published -- the second server's run
  // finished, the evidence cleared, the run's state written, the first run
  // finished -- that fails ends the run, saying where.
  for (const [fault, place] of [
    ['fs.symlink_metadata#javascript_run.rs:1292', 'javascript_run.rs:1292'],
    ['fs.symlink_metadata#javascript_run.rs:1308', 'javascript_run.rs:1308'],
    ['fs.symlink_metadata#javascript_run.rs:1310', 'javascript_run.rs:1310'],
    ['fs.read#javascript_run.rs:1336', 'javascript_run.rs:1336'],
    ['fs.symlink_metadata#javascript_run.rs:1348', 'javascript_run.rs:1348'],
  ]) {
    const failed = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'both servers answer'], { SUPERCOV_TEST_FAULT: fault });
    assert.notEqual(failed.status, 0, `${fault}: ${failed.stdout}${failed.stderr}`);
    assert.ok(failed.stderr.includes(`test fault at fs (${place}`), `${fault}: ${failed.stderr}`);
  }
  // Each step of a server's own publication that fails -- its work
  // directory, its archive, its run moved into place -- is said, and never
  // panics; the first server's failing ends the run.
  for (const [fault, place] of [
    ['fs.create_dir_all#python_run.rs:809', 'python_run.rs:809'],
    ['fs.create_dir_all#ruby_run.rs:642', 'ruby_run.rs:642'],
    ['fs.rename#evidence_archive.rs:471', 'evidence_archive.rs:471'],
    ['fs.rename#evidence_archive.rs:471@2', 'evidence_archive.rs:471'],
    ['fs.rename#lifecycle.rs:1114', 'lifecycle.rs:1114'],
    ['fs.rename#lifecycle.rs:1114@2', 'lifecycle.rs:1114'],
  ]) {
    const failed = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'both servers answer'], { SUPERCOV_TEST_FAULT: fault });
    assert.ok(failed.stderr.includes(`test fault at fs (${place}`), `${fault}: ${failed.stderr}`);
    assert.ok(!failed.stderr.includes('panicked'), `${fault}: ${failed.stderr}`);
  }
  // A failing test still publishes what it reached, and the run says it failed.
  const failing = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'not given']);
  assert.equal(failing.status, 1, failing.stdout + failing.stderr);
  const failed = JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0];
  assert.notEqual(failed.id, bothId, failing.stderr);
  assert.match(run(binary, ['runs', failed.id]).stdout, /status: wrapped command exited 1/u);
  // No test reached a server: nothing to measure, and it says so.
  const idle = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'reaches no server']);
  assert.notEqual(idle.status, 0, idle.stdout + idle.stderr);
  assert.match(idle.stderr, /no test's request reached measured server code/u, idle.stderr);
  // No test ran at all: the command failed before any outcome.
  const none = run(binary, ['--', 'npx', 'playwright', 'test', '-c', 'both.config.js', '-g', 'no such test']);
  assert.notEqual(none.status, 0, none.stdout + none.stderr);
  assert.match(none.stderr, /exited 1 before any test reported an outcome/u, none.stderr);
  console.log('[ruby-server-playwright] a Ruby server under browser tests is measured, each request credited to its test');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'ruby-server-playwright-'))) rmSync(root, { recursive: true, force: true });
}
