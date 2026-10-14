// Browser tests of a Python server measure the server.
//
//   node scripts/python-server-playwright.mjs
//
// A Playwright suite whose `webServer` is a Python program: the project has
// no JavaScript source, and the run is the server's Python run, each request
// credited to the test that sent it -- by header from the request fixture,
// by cookie from a page. A project with a Node server too keeps its browser
// run as the latest and publishes the server's beside it, and merging runs of
// both merges each. Then the servers Python teams run: Flask, FastAPI under
// uvicorn, Django under its own server and under gunicorn and uvicorn, from a
// virtual environment inside the project, with the project installed
// editable.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const python = process.env.SUPERCOV_PYTHON ?? (process.platform === 'win32' ? 'python' : 'python3');
// Inside the repository, so the suite finds its Playwright.
const base = resolve(repository, 'target', `python-server-playwright-${process.pid}-${Date.now()}`);
const port = 46000 + Math.floor(Math.random() * 2000);

const write = (root, files) => {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
};
const run = (root, command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const measure = (root, args, env) => {
  const measured = run(root, binary, ['--', 'npx', 'playwright', 'test', ...args], env);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  return measured;
};
const runs = (root) => JSON.parse(run(root, binary, ['runs', '--json']).stdout).data.runs.map((entry) => entry.id);
const page = (root, id, ...query) => {
  const result = run(root, binary, ['runs', id, ...query]);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout;
};

const grades = 'def grade(score):\n    if score >= 90:\n        return "A"\n    if score >= 50:\n        return "pass"\n    return "fail"\n\n\ndef unused():\n    return "never"\n';
const server = `import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from app.grades import grade


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/grade":
            body, kind = grade(int(parse_qs(url.query)["score"][0])), "text/plain"
        elif url.path == "/":
            body = "<html><body><script>fetch('/grade?score=95').then((r) => r.text()).then((t) => { document.body.dataset.top = t; });</script></body></html>"
            kind = "text/html"
        else:
            body, kind = "ready", "text/plain"
        data = body.encode()
        self.send_response(200)
        self.send_header("content-type", kind)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
`;
const specs = {
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
  const graded = spawnSync(${JSON.stringify(python)}, ['-c', 'from app.grades import grade; print(grade(70))'], { encoding: 'utf8' });
  expect(graded.stdout.trim()).toBe('pass');
});
`,
};
const config = (servers, baseURL) => `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: '${baseURL}' },
  webServer: ${JSON.stringify(servers)},
};
`;

try {
  // A Python server and no JavaScript source.
  const plain = resolve(base, 'plain');
  write(plain, {
    'package.json': '{ "name": "python-server", "private": true, "type": "module" }\n',
    'app/__init__.py': '',
    'app/grades.py': grades,
    'app/server.py': server,
    'playwright.config.js': config([{ command: `${python} -m app.server ${port}`, url: `http://127.0.0.1:${port}/ready`, reuseExistingServer: false }], `http://127.0.0.1:${port}`),
    ...specs,
  });
  const measured = measure(plain, []);
  assert.match(measured.stderr, /Python server coverage: 5 test\(s\) across 3 source file\(s\)/u, measured.stderr);
  const [id] = runs(plain);
  const summary = page(plain, id);
  assert.match(summary, /command: npx playwright test/u, summary);
  assert.match(summary, /Lines {6}96\.55% \(28\/29\)/u, summary);
  assert.match(summary, /Instrumentation {2}Complete/u, summary);
  const lines = (name) => page(plain, id, 'test', name).match(/app\/grades\.py:\d+/gu) ?? [];
  // Each request is its test's: by header from the request fixture, by
  // cookie from a page, and a Python process a test starts is that test's.
  assert.deepEqual(lines('a high score is an A'), ['app/grades.py:1', 'app/grades.py:2', 'app/grades.py:3']);
  assert.deepEqual(lines('a low score fails'), ['app/grades.py:1', 'app/grades.py:2', 'app/grades.py:4', 'app/grades.py:6']);
  assert.deepEqual(lines('the page asks for the top grade'), ['app/grades.py:1', 'app/grades.py:2', 'app/grades.py:3']);
  // Its module loads there too: both definitions ran under it.
  assert.deepEqual(lines('a script the test runs'), ['app/grades.py:1', 'app/grades.py:2', 'app/grades.py:4', 'app/grades.py:5', 'app/grades.py:9']);
  assert.deepEqual(lines('a test that sends nothing'), []);
  // A change the server's tests ran reaches those tests, and no other.
  write(plain, { 'app/grades.py': grades.replace('return "fail"', 'return "failed"') });
  const affected = page(plain, id, 'tests', 'affected', '--names');
  assert.deepEqual(affected.trim().split('\n').sort(), ['grades.spec.js a high score is an A', 'grades.spec.js a low score fails', 'grades.spec.js a script the test runs', 'grades.spec.js the page asks for the top grade'], affected);
  write(plain, { 'app/grades.py': grades });

  // A webhook a mock Supercov does not measure sends: the request names no
  // test, and its work is the test's that was running when it came.
  const webhook = resolve(base, 'webhook');
  const stripe = port + 4;
  write(webhook, {
    'package.json': '{ "name": "python-webhook", "private": true, "type": "module" }\n',
    'app/__init__.py': '',
    'app/payments.py': 'def settle(event):\n    if event.get("type") == "charge.succeeded":\n        return "paid"\n    return "ignored"\n',
    'app/server.py': `import json
import sys
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from app.payments import settle

state = {"order": "none"}


class Handler(BaseHTTPRequestHandler):
    def reply(self, body):
        data = body.encode()
        self.send_response(200)
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/checkout":
            state["order"] = "pending"
            urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:${stripe}/charge", method="POST")).read()
            return self.reply("started")
        if self.path == "/state":
            # Slow, so a poll of the test's runs on another thread when the
            # webhook comes, and leaves while it runs.
            time.sleep(0.2)
            return self.reply(state["order"])
        self.reply("ready")

    def do_POST(self):
        event = json.loads(self.rfile.read(int(self.headers["content-length"])))
        state["order"] = settle(event)
        self.reply("ok")

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
`,
    // Answers the charge, then calls the app's webhook by itself.
    'mocks/stripe.mjs': `import { createServer } from 'node:http';\n\ncreateServer((request, response) => {\n  if (request.url === '/charge') {\n    response.end('{}');\n    setTimeout(() => fetch('http://127.0.0.1:${port + 5}/webhook', { method: 'POST', body: JSON.stringify({ type: 'charge.succeeded' }) }), 100);\n    return;\n  }\n  response.end('ready');\n}).listen(${stripe}, '127.0.0.1');\n`,
    'playwright.config.js': config([
      { command: `${python} -m app.server ${port + 5}`, url: `http://127.0.0.1:${port + 5}/ready`, reuseExistingServer: false },
      // Started without Supercov, as an external mock binary would be.
      { command: 'env -u NODE_OPTIONS node mocks/stripe.mjs', url: `http://127.0.0.1:${stripe}/`, reuseExistingServer: false },
    ], `http://127.0.0.1:${port + 5}`),
    'tests/checkout.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a checkout is paid when the webhook arrives', async ({ request }) => {\n  await request.get('/checkout');\n  await expect.poll(async () => (await request.get('/state')).text()).toBe('paid');\n});\n\ntest('the state can be read', async ({ request }) => {\n  expect(await (await request.get('/state')).text()).toBeTruthy();\n});\n",
  });
  if (process.platform !== 'win32') {
    measure(webhook, []);
    const [paidRun] = runs(webhook);
    const paid = page(webhook, paidRun, 'test', 'a checkout is paid when the webhook arrives');
    assert.match(paid, /app\/payments\.py:3/u, `the webhook is not the checkout test's:\n${paid}`);
    assert.match(paid, /credited by time/u, paid);
    const read = page(webhook, paidRun, 'test', 'the state can be read');
    assert.doesNotMatch(read, /payments\.py|credited by time/u, read);
    assert.match(page(webhook, paidRun), /Attribution {6}1 exact, 1 with server work credited by time/u);
  }

  // A Node server beside it: the browser run is the run, and the server's
  // is published beside it. Merging two runs of each merges each.
  const mixed = resolve(base, 'mixed');
  write(mixed, {
    'package.json': '{ "name": "python-and-node", "private": true, "type": "module" }\n',
    'app/__init__.py': '',
    'app/grades.py': grades,
    'app/server.py': server,
    'src/front.js': `import { createServer } from 'node:http';\n\nexport function label(grade) {\n  return grade === 'A' ? 'top' : 'other';\n}\n\ncreateServer(async (request, response) => {\n  if (request.url === '/ready') return response.end('ready');\n  response.end(label(await (await fetch('http://127.0.0.1:${port + 1}/grade?score=95')).text()));\n}).listen(${port + 2}, '127.0.0.1');\n`,
    'playwright.config.js': config([
      { command: `${python} -m app.server ${port + 1}`, url: `http://127.0.0.1:${port + 1}/ready`, reuseExistingServer: false },
      { command: 'node src/front.js', url: `http://127.0.0.1:${port + 2}/ready`, reuseExistingServer: false },
    ], `http://127.0.0.1:${port + 1}`),
    'tests/grades.spec.js': specs['tests/grades.spec.js'],
    'tests/front.spec.js': `import { expect, test } from '@playwright/test';\n\ntest('the front labels the grade', async ({ request }) => {\n  expect(await (await request.get('http://127.0.0.1:${port + 2}/label')).text()).toBe('top');\n});\n`,
  });
  for (const file of ['tests/grades.spec.js', 'tests/front.spec.js']) {
    const both = measure(mixed, [file]);
    const server = both.stderr.match(/the Python server's coverage is run (run_[0-9a-f]{16})/u)?.[1];
    assert.ok(server, both.stderr);
    const [latest] = runs(mixed);
    assert.notEqual(latest, server, 'the browser run is the latest run');
    // Each names the other where it is read, and in JSON.
    assert.match(page(mixed, latest), new RegExp(`Python server: run ${server}`, 'u'));
    assert.match(page(mixed, server), new RegExp(`tests from: the browser run ${latest}`, 'u'));
    const linked = JSON.parse(run(mixed, binary, ['runs', latest, '--json']).stdout).data.companions;
    assert.deepEqual(linked, [{ id: server, language: 'Python', role: 'server' }]);
  }
  // The front's request reached the Python server through the Node server,
  // which carried the test along.
  const [, serverRun] = runs(mixed);
  assert.match(page(mixed, serverRun, 'test', 'the front labels the grade'), /app\/grades\.py:3/u);
  const merged = run(mixed, binary, ['merge']);
  assert.equal(merged.status, 0, merged.stdout + merged.stderr);
  assert.equal(merged.stdout.match(/\[supercov\] merged run/gu)?.length, 2, merged.stdout);

  // Frameworks and servers, from a virtual environment in the project.
  if (process.platform !== 'win32' && !process.env.SUPERCOV_SKIP_PYTHON_FRAMEWORKS) {
    const frameworks = resolve(base, 'frameworks');
    write(frameworks, {
      'package.json': '{ "name": "python-frameworks", "private": true, "type": "module" }\n',
      'pyproject.toml': '[project]\nname = "gradebook"\nversion = "0.0.0"\n\n[build-system]\nrequires = ["setuptools"]\nbuild-backend = "setuptools.build_meta"\n\n[tool.setuptools.packages.find]\nwhere = ["src"]\n',
      'src/gradebook/__init__.py': grades,
      'app/__init__.py': '',
      'app/flask_app.py': 'from flask import Flask, request\n\nfrom gradebook import grade\n\napp = Flask(__name__)\n\n\n@app.get("/ready")\ndef ready():\n    return "ready"\n\n\n@app.get("/grade")\ndef graded():\n    return grade(int(request.args["score"]))\n',
      'app/fastapi_app.py': 'from fastapi import FastAPI\nfrom fastapi.responses import PlainTextResponse\n\nfrom gradebook import grade\n\napp = FastAPI()\n\n\n@app.get("/ready", response_class=PlainTextResponse)\nasync def ready():\n    return "ready"\n\n\n@app.get("/grade", response_class=PlainTextResponse)\nasync def graded(score: int):\n    return grade(score)\n',
      'app/django_app.py': 'import sys\n\nfrom django.conf import settings\nfrom django.http import HttpResponse\nfrom django.urls import path\n\nfrom gradebook import grade\n\nsettings.configure(DEBUG=True, ROOT_URLCONF=__name__, ALLOWED_HOSTS=["*"], SECRET_KEY="fixture")\n\n\ndef ready(request):\n    return HttpResponse("ready")\n\n\ndef graded(request):\n    return HttpResponse(grade(int(request.GET["score"])))\n\n\nurlpatterns = [path("ready", ready), path("grade", graded)]\n\nif __name__ == "__main__":\n    from django.core.management import execute_from_command_line\n\n    execute_from_command_line([sys.argv[0], "runserver", "--noreload", sys.argv[1]])\n',
      'app/django_wsgi.py': 'from django.core.wsgi import get_wsgi_application\n\nimport app.django_app  # noqa: F401\n\napplication = get_wsgi_application()\n',
      'app/django_asgi.py': 'from django.core.asgi import get_asgi_application\n\nimport app.django_app  # noqa: F401\n\napplication = get_asgi_application()\n',
      'playwright.config.js': `const at = '127.0.0.1:${port + 3}';\nconst servers = {\n  flask: \`.venv/bin/python -m flask --app app.flask_app run --port ${port + 3}\`,\n  fastapi: \`.venv/bin/python -m uvicorn app.fastapi_app:app --port ${port + 3}\`,\n  django: \`.venv/bin/python -m app.django_app \${at}\`,\n  'django-gunicorn': \`.venv/bin/gunicorn -w 2 --threads 2 -b \${at} app.django_wsgi:application\`,\n  'django-uvicorn': \`.venv/bin/python -m uvicorn app.django_asgi:application --port ${port + 3}\`,\n};\nexport default {\n  testDir: 'tests',\n  workers: 1,\n  reporter: 'line',\n  use: { baseURL: \`http://\${at}\` },\n  webServer: { command: servers[process.env.SERVER], url: \`http://\${at}/ready\`, reuseExistingServer: false },\n};\n`,
      'tests/grades.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a high score is an A', async ({ request }) => {\n  expect(await (await request.get('/grade?score=95')).text()).toBe('A');\n});\n\ntest('a low score fails', async ({ request }) => {\n  expect(await (await request.get('/grade?score=10')).text()).toBe('fail');\n});\n",
    });
    const packages = ['flask', 'fastapi', 'uvicorn', 'django', 'gunicorn'];
    // uv where it is installed; pip, upgraded first, where it is not.
    const uv = spawnSync('uv', ['--version']).status === 0;
    for (const step of uv
      ? [
          ['uv', ['venv', '-q', '--python', python, '.venv']],
          ['uv', ['pip', 'install', '-q', '--python', '.venv/bin/python', ...packages]],
          ['uv', ['pip', 'install', '-q', '--python', '.venv/bin/python', '-e', '.']],
        ]
      : [
          [python, ['-m', 'venv', '.venv']],
          ['.venv/bin/python', ['-m', 'pip', 'install', '-q', '--upgrade', 'pip']],
          ['.venv/bin/python', ['-m', 'pip', 'install', '-q', ...packages]],
          ['.venv/bin/python', ['-m', 'pip', 'install', '-q', '-e', '.']],
        ]) {
      const done = run(frameworks, ...step);
      assert.equal(done.status, 0, `${step.flat().join(' ')}: ${done.stdout}${done.stderr}`);
    }
    for (const name of ['flask', 'fastapi', 'django', 'django-gunicorn', 'django-uvicorn']) {
      measure(frameworks, [], { SERVER: name });
      const [latest] = runs(frameworks);
      const low = page(frameworks, latest, 'test', 'a low score fails').match(/src\/gradebook\/__init__\.py:\d+/gu) ?? [];
      assert.deepEqual(low, ['src/gradebook/__init__.py:1', 'src/gradebook/__init__.py:2', 'src/gradebook/__init__.py:4', 'src/gradebook/__init__.py:6'], `${name}: the request is not its test's`);
      const high = page(frameworks, latest, 'test', 'a high score is an A').match(/src\/gradebook\/__init__\.py:\d+/gu) ?? [];
      assert.deepEqual(high, ['src/gradebook/__init__.py:1', 'src/gradebook/__init__.py:2', 'src/gradebook/__init__.py:3'], `${name}: the request is not its test's`);
    }
  }
  console.log('[python-server-playwright] a Python server under browser tests is measured, each request credited to its test');
} finally {
  if (!process.env.KEEP && base.startsWith(resolve(repository, 'target', 'python-server-playwright-'))) rmSync(base, { recursive: true, force: true });
}
