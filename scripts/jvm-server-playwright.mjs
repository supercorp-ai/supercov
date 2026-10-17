// Browser tests of a Java server measure the server.
//
//   node scripts/jvm-server-playwright.mjs
//
// A Playwright suite whose `webServer` compiles and starts a Java server with
// the JDK's own HTTP server: the copy the suite runs in has its Java
// rewritten with probes, and the runtime in the main source set sweeps what
// ran on a clock, each window credited to the browser test whose span it
// falls in, by time. Two tests at once on two workers are neither's.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { freePorts } from './free-ports.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
// Inside the repository, so the suite finds its Playwright.
const root = resolve(repository, 'target', `jvm-server-playwright-${process.pid}-${Date.now()}`);
const [port] = await freePorts(1);

const files = {
  'package.json': '{ "name": "jvm-server", "private": true, "type": "module" }\n',
  'src/main/java/app/Grades.java': 'package app;\n\npublic final class Grades {\n  private Grades() {}\n\n  public static String grade(int score) {\n    if (score >= 90) {\n      return "A";\n    }\n    if (score >= 50) {\n      return "pass";\n    }\n    return "fail";\n  }\n}\n',
  'src/main/java/app/Server.java': `package app;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;

public final class Server {
  private static final java.util.concurrent.CountDownLatch BOTH = new java.util.concurrent.CountDownLatch(2);

  public static void main(String[] args) throws Exception {
    HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", Integer.parseInt(args[0])), 0);
    server.setExecutor(java.util.concurrent.Executors.newFixedThreadPool(4));
    server.createContext("/ready", exchange -> reply(exchange, "ready"));
    server.createContext("/grade", exchange -> {
      String query = exchange.getRequestURI().getQuery();
      // Two slow requests meet before either grades, and each holds its
      // reply after: each grade runs while both tests are running, however
      // late the second worker starts.
      boolean slow = query.contains("slow");
      try {
        if (slow) {
          BOTH.countDown();
          BOTH.await(20, java.util.concurrent.TimeUnit.SECONDS);
        }
      } catch (InterruptedException interrupted) {
        Thread.currentThread().interrupt();
      }
      int score = Integer.parseInt(query.replaceAll(".*score=([0-9]+).*", "$1"));
      String grade = Grades.grade(score);
      try {
        if (slow) {
          Thread.sleep(300);
        }
      } catch (InterruptedException interrupted) {
        Thread.currentThread().interrupt();
      }
      reply(exchange, grade);
    });
    server.start();
  }

  private static void reply(HttpExchange exchange, String body) throws IOException {
    byte[] bytes = body.getBytes(StandardCharsets.UTF_8);
    exchange.sendResponseHeaders(200, bytes.length);
    exchange.getResponseBody().write(bytes);
    exchange.close();
  }
}
`,
  'playwright.config.js': `export default {
  testDir: process.env.TESTS ?? 'tests',
  workers: Number(process.env.WORKERS ?? 1),
  fullyParallel: true,
  reporter: 'line',
  use: { baseURL: 'http://127.0.0.1:${port}' },
  webServer: { command: 'rm -rf out && javac -d out $(find src/main/java -name "*.java") && java -cp out app.Server ${port}', url: 'http://127.0.0.1:${port}/ready', reuseExistingServer: false, timeout: 180000 },
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
const lines = (id, name) => run(binary, ['runs', id, 'test', name]).stdout.match(/src\/main\/java\/app\/Grades\.java:\d+/gu) ?? [];

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  const id = measure({});
  assert.deepEqual(lines(id, 'a high score is an A'), ['src/main/java/app/Grades.java:6', 'src/main/java/app/Grades.java:8']);
  assert.deepEqual(lines(id, 'a low score fails'), ['src/main/java/app/Grades.java:6', 'src/main/java/app/Grades.java:13']);
  assert.deepEqual(lines(id, 'a test that sends nothing'), []);
  const summary = run(binary, ['runs', id]).stdout;
  assert.match(summary, /Attribution {6}1 exact, 2 with server work credited by time/u, summary);
  assert.match(run(binary, ['runs', id, 'test', 'a low score fails']).stdout, /credited by time/u);
  // Two tests at once: what the server ran is the run's, and neither's.
  const together = measure({ TESTS: 'together', WORKERS: '2' });
  assert.deepEqual(lines(together, 'one at once'), [], 'work done while two tests ran was given to one');
  assert.deepEqual(lines(together, 'another at once'), []);
  assert.match(run(binary, ['runs', together]).stdout, /no test {20}lines/u);
  console.log('[jvm-server-playwright] a Java server under browser tests is measured, each window credited by time to the test it fell in');
} finally {
  if (!process.env.KEEP && root.startsWith(resolve(repository, 'target', 'jvm-server-playwright-'))) rmSync(root, { recursive: true, force: true });
}
