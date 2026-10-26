// Browser tests of a server compose builds from the copy measure the server.
//
//   node scripts/docker-server-playwright.mjs
//
// A Playwright suite whose `webServer` is `docker compose up --build`: the
// image is built from the copy the suite runs in, so it holds the copy's
// instrumented Go, and Supercov's `docker` launcher gives the service the
// environment and directory its windows need. Each window is credited, by
// time, to the browser test it fell in, as for a server started with
// `go run`. Without Docker there is nothing to run, and the gate says so.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `docker-server-playwright-${process.pid}-${Date.now()}`);
// A port nothing holds now: other processes on a developer's machine take
// random high ones.
const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});

const docker = spawnSync('docker', ['info', '--format', '{{.ServerVersion}}'], { encoding: 'utf8' });
if (process.platform === 'win32' || docker.status !== 0) {
  console.log('[docker-server-playwright] skipped: no Docker daemon here');
  process.exit(0);
}

const shared = (port, project) => ({
  'package.json': '{ "name": "docker-server", "private": true, "type": "module" }\n',
  'compose.yaml': `name: ${project}\nservices:\n  api:\n    build: .\n    ports:\n      - "127.0.0.1:${port}:8080"\n  cache:\n    image: alpine:3.22\n    command: ["sleep", "600"]\n`,
  'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'docker compose up --build', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false, timeout: 600000 },
};
`,
  'tests/grades.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a high score is an A', async ({ request }) => {\n  // Server work is credited by the window it falls in; a pause on each\n  // side keeps it from sharing one with the test before or after.\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  expect(await (await request.get('/grade?score=95')).text()).toBe('A');\n  await new Promise((resolve) => setTimeout(resolve, 50));\n});\n\ntest('a low score fails', async ({ request }) => {\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  expect(await (await request.get('/grade?score=10')).text()).toBe('fail');\n  await new Promise((resolve) => setTimeout(resolve, 50));\n});\n\ntest('a test that sends nothing', async () => {\n  expect(1).toBe(1);\n});\n",
});

const cases = [
  {
    language: 'Go',
    source: 'internal/grades/grades.go',
    high: [3, 4, 5],
    low: [3, 4, 7, 10],
    files: {
      'go.mod': 'module example.com/gradebook\n\ngo 1.22\n',
      'internal/grades/grades.go': 'package grades\n\nfunc Grade(score int) string {\n\tif score >= 90 {\n\t\treturn "A"\n\t}\n\tif score >= 50 {\n\t\treturn "pass"\n\t}\n\treturn "fail"\n}\n',
      'cmd/server/main.go': 'package main\n\nimport (\n\t"net/http"\n\t"strconv"\n\n\t"example.com/gradebook/internal/grades"\n)\n\nfunc main() {\n\thttp.HandleFunc("/ready", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ready")) })\n\thttp.HandleFunc("/grade", func(w http.ResponseWriter, r *http.Request) {\n\t\tscore, _ := strconv.Atoi(r.URL.Query().Get("score"))\n\t\tw.Write([]byte(grades.Grade(score)))\n\t})\n\thttp.ListenAndServe("0.0.0.0:8080", nil)\n}\n',
      // As a project ships it: built in one stage, run from a small image.
      Dockerfile: 'FROM golang:1.27-alpine AS build\nWORKDIR /src\nCOPY go.mod ./\nRUN go mod download\nCOPY . .\nRUN CGO_ENABLED=0 go build -o /server ./cmd/server\n\nFROM alpine:3.22\nCOPY --from=build /server /server\nEXPOSE 8080\nENTRYPOINT ["/server"]\n',
    },
  },
  {
    language: 'Java/Kotlin',
    source: 'src/main/java/app/Grades.java',
    high: [6, 8],
    low: [6, 13],
    files: {
      'src/main/java/app/Grades.java': 'package app;\n\npublic final class Grades {\n  private Grades() {}\n\n  public static String grade(int score) {\n    if (score >= 90) {\n      return "A";\n    }\n    if (score >= 50) {\n      return "pass";\n    }\n    return "fail";\n  }\n}\n',
      'src/main/java/app/Server.java': 'package app;\n\nimport com.sun.net.httpserver.HttpExchange;\nimport com.sun.net.httpserver.HttpServer;\nimport java.io.IOException;\nimport java.net.InetSocketAddress;\nimport java.nio.charset.StandardCharsets;\n\npublic final class Server {\n  public static void main(String[] args) throws Exception {\n    HttpServer server = HttpServer.create(new InetSocketAddress("0.0.0.0", 8080), 0);\n    server.createContext("/ready", exchange -> reply(exchange, "ready"));\n    server.createContext("/grade", exchange -> {\n      String query = exchange.getRequestURI().getQuery();\n      int score = Integer.parseInt(query.replaceAll(".*score=([0-9]+).*", "$1"));\n      reply(exchange, Grades.grade(score));\n    });\n    server.start();\n  }\n\n  private static void reply(HttpExchange exchange, String body) throws IOException {\n    byte[] bytes = body.getBytes(StandardCharsets.UTF_8);\n    exchange.sendResponseHeaders(200, bytes.length);\n    exchange.getResponseBody().write(bytes);\n    exchange.close();\n  }\n}\n',
      Dockerfile: 'FROM eclipse-temurin:21-jdk AS build\nWORKDIR /src\nCOPY . .\nRUN javac -d /out $(find src/main/java -name "*.java")\n\nFROM eclipse-temurin:21-jre\nCOPY --from=build /out /app\nEXPOSE 8080\nENTRYPOINT ["java", "-cp", "/app", "app.Server"]\n',
    },
  },
  {
    language: 'JavaScript',
    // A Node server's evidence is the browser run's own, credited per
    // request to the test that sent it.
    serverRun: false,
    source: 'src/grades.js',
    high: [1, 2, 3],
    low: [1, 2, 5, 8],
    files: {
      'src/grades.js': 'export function grade(score) {\n  if (score >= 90) {\n    return "A";\n  }\n  if (score >= 50) {\n    return "pass";\n  }\n  return "fail";\n}\n',
      'server.mjs': "import { createServer } from 'node:http';\nimport { grade } from './src/grades.js';\n\ncreateServer((request, response) => {\n  const url = new URL(request.url, 'http://x');\n  if (url.pathname === '/ready') return response.end('ready');\n  response.end(grade(Number(url.searchParams.get('score'))));\n}).listen(8080, '0.0.0.0');\n",
      Dockerfile: 'FROM node:24-alpine\nWORKDIR /app\nCOPY . .\nEXPOSE 8080\nCMD ["node", "server.mjs"]\n',
    },
  },
  {
    language: 'Python',
    // The Dockerfile moves the code (`COPY backend/ ./`), as projects do: the
    // image's `/srv/app/grades.py` is the copy's `backend/app/grades.py`.
    source: 'backend/app/grades.py',
    high: [1, 2, 3],
    low: [1, 2, 4, 6],
    files: {
      'backend/app/__init__.py': '',
      'backend/app/grades.py': 'def grade(score):\n    if score >= 90:\n        return "A"\n    if score >= 50:\n        return "pass"\n    return "fail"\n',
      'backend/server.py': 'from http.server import BaseHTTPRequestHandler, HTTPServer\nfrom urllib.parse import parse_qs, urlparse\n\nfrom app.grades import grade\n\n\nclass Handler(BaseHTTPRequestHandler):\n    def do_GET(self):\n        url = urlparse(self.path)\n        body = "ready" if url.path == "/ready" else grade(int(parse_qs(url.query)["score"][0]))\n        self.send_response(200)\n        self.end_headers()\n        self.wfile.write(body.encode())\n\n\nHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()\n',
      Dockerfile: 'FROM python:3.12-slim\nWORKDIR /srv\nCOPY backend/ ./\nEXPOSE 8080\nCMD ["python", "server.py"]\n',
    },
  },
  {
    language: 'Ruby',
    source: 'lib/grades.rb',
    high: [2, 3, 4],
    low: [2, 3, 8],
    files: {
      'lib/grades.rb': 'module Grades\n  def self.grade(score)\n    if score >= 90\n      "A"\n    elsif score >= 50\n      "pass"\n    else\n      "fail"\n    end\n  end\nend\n',
      'server.rb': 'require "webrick"\nrequire_relative "lib/grades"\n\nserver = WEBrick::HTTPServer.new(BindAddress: "0.0.0.0", Port: 8080, AccessLog: [], Logger: WEBrick::Log.new(File::NULL))\nserver.mount_proc("/ready") { |_request, response| response.body = "ready" }\nserver.mount_proc("/grade") { |request, response| response.body = Grades.grade(Integer(request.query["score"])) }\ntrap("TERM") { server.shutdown }\nserver.start\n',
      Dockerfile: 'FROM ruby:3.4-slim\nRUN gem install webrick --no-document\nWORKDIR /app\nCOPY . .\nEXPOSE 8080\nCMD ["ruby", "server.rb"]\n',
    },
  },
];

const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const lines = (cwd, id, name, source) => run(cwd, binary, ['runs', id, 'test', name]).stdout.match(new RegExp(`${source.replace(/[.]/g, '\\.')}:\\d+`, 'gu')) ?? [];

const projects = [];
try {
  for (const [index, entry] of cases.entries()) {
    if (process.env.ONLY && entry.language !== process.env.ONLY) continue;
    const cwd = resolve(root, entry.language.replace(/\W/g, '').toLowerCase());
    const project = `supercov-docker-gate-${process.pid}-${index}`;
    projects.push(project);
    const port = await freePort();
    for (const [file, text] of Object.entries({ ...shared(port, project), ...entry.files })) {
      mkdirSync(dirname(resolve(cwd, file)), { recursive: true });
      writeFileSync(resolve(cwd, file), text);
    }
    const measured = run(cwd, binary, ['--', 'npx', 'playwright', 'test']);
    assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
    if (entry.serverRun !== false)
      assert.match(measured.stderr, new RegExp(`${entry.language.replace('/', '\\/')} server coverage: 3 test\\(s\\)`, 'u'), measured.stderr);
    const id = JSON.parse(run(cwd, binary, ['runs', '--json']).stdout).data.runs[0].id;
    const at = (numbers) => numbers.map((line) => `${entry.source}:${line}`);
    assert.deepEqual(lines(cwd, id, 'a high score is an A', entry.source), at(entry.high), entry.language);
    assert.deepEqual(lines(cwd, id, 'a low score fails', entry.source), at(entry.low), entry.language);
    assert.deepEqual(lines(cwd, id, 'a test that sends nothing', entry.source), [], entry.language);
    // The run stopped what compose started, and left it to the suite:
    // stopped, not removed.
    const containers = spawnSync('docker', ['compose', '-p', project, 'ps', '-a', '--format', '{{.Service}} {{.State}}'], { encoding: 'utf8' }).stdout;
    assert.match(containers, /api exited/u, containers);
    assert.doesNotMatch(containers, /running/u, containers);
    // On Linux the container's root owns what it creates in the mount; the
    // run must still be able to remove all of it.
    if (process.platform === 'linux') {
      const closed = spawnSync('find', [resolve(cwd, '.supercov'), '-type', 'd', '!', '-user', String(process.getuid()), '!', '-perm', '-0002', '-printf', '%u %m %p\n'], { encoding: 'utf8' }).stdout.trim();
      assert.equal(closed, '', `${entry.language}: directories the run cannot empty:\n${closed}`);
    }
  }
  // A service whose build context is outside the copy, or whose build from
  // the copy fails (SUPERCOV_TEST_FAULT), is left as compose builds it: the
  // run goes on.
  if (!process.env.ONLY || process.env.ONLY === 'JavaScript') {
    const cwd = resolve(root, 'javascript');
    for (const fault of ['compose-context-outside', 'compose-build']) {
      const measured = run(cwd, binary, ['--', 'npx', 'playwright', 'test'], { SUPERCOV_TEST_FAULT: fault });
      assert.equal(measured.status, 0, `${fault}: ${measured.stdout}${measured.stderr}`);
      assert.ok(measured.stderr.includes(`test fault at ${fault}`), `${fault} was never reached:\n${measured.stderr}`);
    }
  }
  console.log('[docker-server-playwright] Go, Java, Node, Python and Ruby servers compose builds from the copy are measured, each test credited its own work, and left stopped');
} finally {
  for (const project of projects)
    spawnSync('docker', ['compose', '-p', project, 'down', '--rmi', 'local', '--volumes'], { stdio: 'ignore' });
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'docker-server-playwright-'))) {
    try {
      rmSync(root, { recursive: true, force: true });
    } catch {
      // What a container left that this user cannot remove: removed by a
      // container, so the next gate starts clean (the assertion above names it).
      spawnSync('docker', ['run', '--rm', '-v', `${root}:${root}`, 'alpine:3.22', 'rm', '-rf', root], { stdio: 'ignore' });
      rmSync(root, { recursive: true, force: true });
    }
  }
}
