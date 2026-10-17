// Servers any Docker client starts are measured, not only compose's.
//
//   node scripts/docker-clients-playwright.mjs
//
// The suite's Docker clients reach the daemon through Supercov's relay, which
// gives each container whose image the suite built from the copy the run's
// settings. Two clients compose never sees: `docker build && docker run` in
// Playwright's `webServer`, and testcontainers building and starting the
// server from a test (its reaper, Ryuk, mounts the daemon's own socket). Each
// window of the Go server is credited, by time, to the test it fell in.
// Without Docker there is nothing to run, and the gate says so.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const root = resolve(repository, 'target', `docker-clients-playwright-${process.pid}-${Date.now()}`);
const PLAYWRIGHT = JSON.parse(readFileSync(resolve(repository, 'package.json'), 'utf8')).devDependencies['@playwright/test'];
const TESTCONTAINERS = '11.7.1';
const freePort = () => new Promise((done, fail) => {
  const server = createServer();
  server.once('error', fail);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => done(port));
  });
});
const run = (cwd, command, args, env = {}) => spawnSync(command, args, { cwd, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });

const docker = spawnSync('docker', ['info', '--format', '{{.ServerVersion}}'], { encoding: 'utf8' });
if (process.platform === 'win32' || docker.status !== 0) {
  console.log('[docker-clients-playwright] skipped: no Docker daemon here');
  process.exit(0);
}

const go = {
  'go.mod': 'module example.com/gradebook\n\ngo 1.22\n',
  'internal/grades/grades.go': 'package grades\n\nfunc Grade(score int) string {\n\tif score >= 90 {\n\t\treturn "A"\n\t}\n\tif score >= 50 {\n\t\treturn "pass"\n\t}\n\treturn "fail"\n}\n',
  'cmd/server/main.go': 'package main\n\nimport (\n\t"net/http"\n\t"strconv"\n\n\t"example.com/gradebook/internal/grades"\n)\n\nfunc main() {\n\thttp.HandleFunc("/ready", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ready")) })\n\thttp.HandleFunc("/grade", func(w http.ResponseWriter, r *http.Request) {\n\t\tscore, _ := strconv.Atoi(r.URL.Query().Get("score"))\n\t\tw.Write([]byte(grades.Grade(score)))\n\t})\n\thttp.ListenAndServe("0.0.0.0:8080", nil)\n}\n',
  Dockerfile: 'FROM golang:1.27-alpine AS build\nWORKDIR /src\nCOPY go.mod ./\nRUN go mod download\nCOPY . .\nRUN CGO_ENABLED=0 go build -o /server ./cmd/server\n\nFROM alpine:3.22\nCOPY --from=build /server /server\nEXPOSE 8080\nENTRYPOINT ["/server"]\n',
  '.dockerignore': 'node_modules\n.supercov\ntest-results\n',
};
// A pause on each side keeps a test's server work from sharing a window
// with the test before or after.
const tests = (url) => `import { expect, test } from '@playwright/test';
${url.imports}
const pause = () => new Promise((resolve) => setTimeout(resolve, 50));
${url.setup}
test('a high score is an A', async ({ request }) => {
  await pause();
  expect(await (await request.get(${url.base} + '/grade?score=95')).text()).toBe('A');
  await pause();
});

test('a low score fails', async ({ request }) => {
  await pause();
  expect(await (await request.get(${url.base} + '/grade?score=10')).text()).toBe('fail');
  await pause();
});

test('a test that sends nothing', async () => {
  expect(1).toBe(1);
});
`;

const tag = `supercov-docker-clients-${process.pid}`;
const cases = [
  {
    client: 'docker run',
    files: (port) => ({
      'package.json': `{ "name": "docker-run", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}" } }\n`,
      'playwright.config.js': `export default {
  testDir: 'tests',
  workers: 1,
  reporter: 'line',
  webServer: { command: 'docker build -q -t ${tag} . && docker run --rm -p 127.0.0.1:${port}:8080 ${tag}', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false, timeout: 600000 },
};
`,
      'tests/grades.spec.js': tests({ imports: '', setup: '', base: `'http://127.0.0.1:${port}'` }),
    }),
  },
  {
    client: 'testcontainers',
    files: () => ({
      'package.json': `{ "name": "testcontainers-run", "private": true, "type": "module", "devDependencies": { "@playwright/test": "${PLAYWRIGHT}", "testcontainers": "${TESTCONTAINERS}" } }\n`,
      'playwright.config.js': "export default { testDir: 'tests', workers: 1, reporter: 'line', timeout: 600000 };\n",
      'tests/grades.spec.js': tests({
        imports: "import { GenericContainer, Wait } from 'testcontainers';",
        setup: `let server;
let base;
test.beforeAll(async () => {
  test.setTimeout(600000);
  const image = await GenericContainer.fromDockerfile(process.cwd()).build('${tag}-tc', { deleteOnExit: false });
  server = await image.withExposedPorts(8080).withWaitStrategy(Wait.forHttp('/ready', 8080)).start();
  base = \`http://\${server.getHost()}:\${server.getMappedPort(8080)}\`;
});
test.afterAll(async () => {
  await server?.stop();
});
`,
        base: 'base',
      }),
    }),
  },
];

try {
  for (const entry of cases) {
    if (process.env.ONLY && entry.client !== process.env.ONLY) continue;
    const cwd = resolve(root, entry.client.replace(/\W/g, ''));
    const port = await freePort();
    for (const [file, text] of Object.entries({ ...go, ...entry.files(port) })) {
      mkdirSync(dirname(resolve(cwd, file)), { recursive: true });
      writeFileSync(resolve(cwd, file), text);
    }
    const installed = run(cwd, 'npm', ['install', '--no-audit', '--no-fund', '--loglevel', 'error']);
    assert.equal(installed.status, 0, installed.stderr);
    assert.equal(run(cwd, 'git', ['init', '-q']).status, 0);
    const measured = run(cwd, binary, ['--', 'npx', 'playwright', 'test']);
    assert.equal(measured.status, 0, `${entry.client}\n${measured.stdout}${measured.stderr}`);
    assert.match(measured.stderr, /Go server coverage: 3 test\(s\)/u, `${entry.client}\n${measured.stderr}`);
    const id = JSON.parse(run(cwd, binary, ['runs', '--json']).stdout).data.runs[0].id;
    const lines = (name) => run(cwd, binary, ['runs', id, 'test', name]).stdout.match(/internal\/grades\/grades\.go:\d+/gu) ?? [];
    const at = (numbers) => numbers.map((line) => `internal/grades/grades.go:${line}`);
    assert.deepEqual(lines('a high score is an A'), at([3, 4, 5]), entry.client);
    assert.deepEqual(lines('a low score fails'), at([3, 4, 7, 10]), entry.client);
    assert.deepEqual(lines('a test that sends nothing'), [], entry.client);
  }
  console.log('[docker-clients-playwright] a Go server started by `docker run` and by testcontainers is measured, each test credited its own work');
} finally {
  for (const image of [tag, `${tag}-tc`]) spawnSync('docker', ['image', 'rm', '-f', image], { stdio: 'ignore' });
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'docker-clients-playwright-')))
    rmSync(root, { recursive: true, force: true });
}
