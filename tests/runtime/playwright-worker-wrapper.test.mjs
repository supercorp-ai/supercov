import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// A Playwright runner that forks a test worker from its worker entry and
// spawns a web server the way `webServer` does. Each child reports whether
// the wrapper loaded `@playwright/test` into it.
function project(t, workerEntry) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-playwright-worker-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(root, ".supercov/node_modules"), { recursive: true });
  write("package.json", JSON.stringify({ name: "project" }));
  write("playwright.config.mjs", "export default {};\n");
  write("node_modules/@playwright/test/package.json", JSON.stringify({ name: "@playwright/test", main: "index.js" }));
  write("node_modules/@playwright/test/index.js", [
    "require('node:fs').appendFileSync(process.env.LOADED, process.env.ROLE + '\\n');",
    "const test = () => {}; test.extend = () => test; test.describe = () => {};",
    "exports.test = test; exports.expect = () => {}; exports.chromium = {};",
    "",
  ].join("\n"));
  write("node_modules/playwright/cli.js", [
    "const { fork, spawn } = require('node:child_process');",
    "const { resolve } = require('node:path');",
    `const worker = fork(resolve(${JSON.stringify(root)}, ${JSON.stringify(workerEntry)}), [], { env: { ...process.env, ROLE: 'worker' } });`,
    "const server = spawn(process.execPath, [resolve(__dirname, '../../server.js')], { env: { ...process.env, ROLE: 'server' }, stdio: 'inherit' });",
    "let open = 2; const done = () => { if (--open === 0) process.exit(0); };",
    "worker.on('exit', done); server.on('exit', done);",
    "",
  ].join("\n"));
  write(workerEntry, "setTimeout(() => process.exit(0), 50);\n");
  write("server.js", "setTimeout(() => process.exit(0), 50);\n");
  return root;
}

function run(root) {
  const loaded = resolve(root, "loaded.txt");
  writeFileSync(loaded, "");
  const result = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(root, ".supercov/node_modules/register.mjs")).href}`,
    resolve(root, "node_modules/playwright/cli.js"),
  ], {
    cwd: root,
    encoding: "utf8",
    timeout: 20000,
    env: {
      ...process.env,
      NODE_OPTIONS: `--import=${pathToFileURL(resolve(root, ".supercov/node_modules/register.mjs")).href}`,
      SUPERCOV_GENERATED_PLAYWRIGHT_CONFIG: resolve(root, "playwright.config.mjs"),
      SUPERCOV_CJS_INTERCEPT: "1",
      SUPERCOV_PROJECT_ROOT: root,
      ROLE: "runner",
      LOADED: loaded,
    },
  });
  assert.equal(result.status, 0, result.stderr);
  return readFileSync(loaded, "utf8").split("\n").filter(Boolean).sort();
}

test("only the runner's test workers load the Playwright wrapper", t => {
  const root = project(t, "node_modules/playwright/lib/worker/workerProcessEntry.js");
  assert.deepEqual(run(root), ["worker"]);
});

test("a worker installed outside node_modules still loads it", t => {
  // Yarn Plug'n'Play keeps packages in zip archives, with no node_modules path
  // to recognise Playwright's files by: the runner's mark is what tells.
  const root = project(t, ".yarn/unplugged/playwright/lib/worker/workerProcessEntry.js");
  assert.deepEqual(run(root), ["worker"]);
});
