// Browser tests of a Go server measure the server.
//
//   node scripts/go-server-playwright.mjs
//
// A Playwright suite whose `webServer` is `go run ./cmd/server`: the copy the
// suite runs in has its Go rewritten with probes, and the server harvests
// what ran on a clock, each window credited to the browser test whose span
// it falls in, by time. Two tests at once on two workers are neither's.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePorts } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `go-server-playwright-${process.pid}-${Date.now()}`);
const [port] = await freePorts(1);

const files = {
  'package.json': '{ "name": "go-server", "private": true, "type": "module" }\n',
  'go.mod': 'module example.com/gradebook\n\ngo 1.22\n',
  'internal/grades/grades.go': 'package grades\n\nfunc Grade(score int) string {\n\tif score >= 90 {\n\t\treturn "A"\n\t}\n\tif score >= 50 {\n\t\treturn "pass"\n\t}\n\treturn "fail"\n}\n',
  'cmd/server/main.go': `package main

import (
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"

	"example.com/gradebook/internal/grades"
)

// Two slow requests meet before either grades, and each holds its reply
// after: each grade runs while both tests are running, however late the
// second worker starts.
var (
	arrived = make(chan struct{}, 2)
	met     = make(chan struct{})
	meet    sync.Once
)

func main() {
	http.HandleFunc("/ready", func(w http.ResponseWriter, r *http.Request) { w.Write([]byte("ready")) })
	http.HandleFunc("/grade", func(w http.ResponseWriter, r *http.Request) {
		score, _ := strconv.Atoi(r.URL.Query().Get("score"))
		slow := r.URL.Query().Get("slow") != ""
		if slow {
			arrived <- struct{}{}
			if len(arrived) == 2 {
				meet.Do(func() { close(met) })
			}
			select {
			case <-met:
			case <-time.After(20 * time.Second):
			}
		}
		grade := grades.Grade(score)
		if slow {
			time.Sleep(300 * time.Millisecond)
		}
		w.Write([]byte(grade))
	})
	http.ListenAndServe("127.0.0.1:"+os.Args[1], nil)
}
`,
  'playwright.config.js': `export default {
  testDir: process.env.TESTS ?? 'tests',
  workers: Number(process.env.WORKERS ?? 1),
  fullyParallel: true,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'go run ./cmd/server ${port}', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false, timeout: 180000 },
};
`,
  'tests/grades.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('a high score is an A', async ({ request }) => {\n  // Server work is credited by the window it falls in; a pause on each\n  // side keeps it from sharing one with the test before or after.\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  expect(await (await request.get('/grade?score=95')).text()).toBe('A');\n  await new Promise((resolve) => setTimeout(resolve, 50));\n});\n\ntest('a low score fails', async ({ request }) => {\n  await new Promise((resolve) => setTimeout(resolve, 50));\n  expect(await (await request.get('/grade?score=10')).text()).toBe('fail');\n  await new Promise((resolve) => setTimeout(resolve, 50));\n});\n\ntest('a test that sends nothing', async () => {\n  expect(1).toBe(1);\n});\n",
  // Two tests at once, each holding its request open while the other runs.
  'together/together.spec.js': "import { expect, test } from '@playwright/test';\n\ntest('one at once', async ({ request }) => {\n  expect(await (await request.get('/grade?score=95&slow=1')).text()).toBe('A');\n});\n\ntest('another at once', async ({ request }) => {\n  expect(await (await request.get('/grade?score=70&slow=1')).text()).toBe('pass');\n});\n",
};

const run = (command, args, env = {}) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1', ...env } });
const measure = (env) => {
  const measured = run(binary, ['--', 'npx', 'playwright', 'test'], env);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  return JSON.parse(run(binary, ['runs', '--json']).stdout).data.runs[0].id;
};
const lines = (id, name) => run(binary, ['runs', id, 'test', name]).stdout.match(/internal\/grades\/grades\.go:\d+/gu) ?? [];

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const id = measure({});
  assert.deepEqual(lines(id, 'a high score is an A'), ['internal/grades/grades.go:3', 'internal/grades/grades.go:4', 'internal/grades/grades.go:5']);
  assert.deepEqual(lines(id, 'a low score fails'), ['internal/grades/grades.go:3', 'internal/grades/grades.go:4', 'internal/grades/grades.go:7', 'internal/grades/grades.go:10']);
  assert.deepEqual(lines(id, 'a test that sends nothing'), []);
  const summary = run(binary, ['runs', id]).stdout;
  assert.match(summary, /Attribution {6}1 exact, 2 with server work credited by time/u, summary);
  assert.match(run(binary, ['runs', id, 'test', 'a low score fails']).stdout, /credited by time/u);
  // Two tests at once: what the server ran is the run's, and neither's.
  const together = measure({ TESTS: 'together', WORKERS: '2' });
  assert.deepEqual(lines(together, 'one at once'), [], 'work done while two tests ran was given to one');
  assert.deepEqual(lines(together, 'another at once'), []);
  assert.match(run(binary, ['runs', together]).stdout, /no test {20}lines/u);
  console.log('[go-server-playwright] a Go server under browser tests is measured, each window credited by time to the test it fell in');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'go-server-playwright-'))) rmSync(root, { recursive: true, force: true });
}
