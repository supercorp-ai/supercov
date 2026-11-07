import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// What Playwright 1.59 does when it caches a file it compiled: one write in
// place, into a folder of the cache.
test("a file Playwright compiles is written into its cache whole", t => {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-transform-cache-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const runtime = resolve(root, ".supercov/node_modules");
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), runtime, { recursive: true });
  const cache = resolve(root, "cache");
  const script = [
    "const fs = require('node:fs');",
    "const path = require('node:path');",
    "const folder = path.join(process.env.PWTEST_CACHE_DIR, 'ab');",
    "fs.mkdirSync(folder, { recursive: true });",
    "const renamed = [];",
    "const renameSync = fs.renameSync;",
    "fs.renameSync = (from, to) => { renamed.push([path.basename(from), path.basename(to)]); return renameSync(from, to); };",
    "fs.writeFileSync(path.join(folder, 'config_1234567.js'), 'export default {};', 'utf8');",
    "fs.writeFileSync(fs.openSync(path.join(folder, 'by-descriptor'), 'w'), 'kept');",
    "fs.writeFileSync(path.join(process.cwd(), 'outside.txt'), 'plain');",
    "console.log(JSON.stringify(renamed));",
  ].join("\n");
  const ran = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(runtime, "register.mjs")).href}`,
    "-e",
    script,
  ], { cwd: root, encoding: "utf8", timeout: 15000, env: { ...process.env, PWTEST_CACHE_DIR: cache } });
  assert.equal(ran.status, 0, ran.stdout + ran.stderr);
  // Into the cache by a rename, from beside its folders; elsewhere as asked.
  const renamed = JSON.parse(ran.stdout.trim().split("\n").at(-1));
  assert.deepEqual(renamed.map(([, to]) => to), ["config_1234567.js"]);
  assert.match(renamed[0][0], /^\.writing-\d+-0-[0-9a-f-]{36}$/u);
  assert.equal(readFileSync(resolve(cache, "ab/config_1234567.js"), "utf8"), "export default {};");
  assert.equal(readFileSync(resolve(cache, "ab/by-descriptor"), "utf8"), "kept");
  assert.equal(readFileSync(resolve(root, "outside.txt"), "utf8"), "plain");
  assert.deepEqual(readdirSync(cache).sort(), ["ab"]);
});

// And what Playwright does with an ES module: the same write, from the thread
// its loader hooks run in, where the preload never ran. A test file importing
// a module another process was still writing failed at "does not provide an
// export named".
test("a module Playwright compiles in its loader thread is written into its cache whole", t => {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-transform-cache-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const runtime = resolve(root, ".supercov/node_modules");
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), runtime, { recursive: true });
  const cache = resolve(root, "cache");
  // Stands in for Playwright's loader: registered after Supercov's, it
  // shares that thread, and writes as the cache does on its first load.
  writeFileSync(resolve(root, "hooks.mjs"), [
    "import fs from 'node:fs';",
    "import path from 'node:path';",
    "import { threadId } from 'node:worker_threads';",
    "let port;",
    "export function initialize(data) { port = data.port; }",
    "export async function load(url, context, next) {",
    "  if (!url.endsWith('/esm.mjs')) return next(url, context);",
    "  const folder = path.join(process.env.PWTEST_CACHE_DIR, 'cd');",
    "  fs.mkdirSync(folder, { recursive: true });",
    "  const renameSync = fs.renameSync;",
    "  const renamed = [];",
    "  fs.renameSync = (from, to) => { renamed.push([path.basename(from), path.basename(to)]); return renameSync(from, to); };",
    "  fs.writeFileSync(path.join(folder, 'esm_7654321.js'), 'export const x = 1;', 'utf8');",
    "  fs.renameSync = renameSync;",
    "  port.postMessage({ threadId, renamed });",
    "  return next(url, context);",
    "}",
  ].join("\n"));
  writeFileSync(resolve(root, "esm.mjs"), "export const x = 1;\n");
  const script = [
    "import { register } from 'node:module';",
    "import { MessageChannel, threadId } from 'node:worker_threads';",
    "const { port1, port2 } = new MessageChannel();",
    "const seen = new Promise((done) => port1.once('message', done));",
    "register('./hooks.mjs', { parentURL: import.meta.url, data: { port: port2 }, transferList: [port2] });",
    "await import('./esm.mjs');",
    "const message = await seen;",
    "port1.close();",
    "console.log(JSON.stringify({ main: threadId, ...message }));",
  ].join("\n");
  writeFileSync(resolve(root, "main.mjs"), script);
  const ran = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(runtime, "register.mjs")).href}`,
    "main.mjs",
  ], { cwd: root, encoding: "utf8", timeout: 15000, env: { ...process.env, PWTEST_CACHE_DIR: cache } });
  assert.equal(ran.status, 0, ran.stdout + ran.stderr);
  const { main, threadId, renamed } = JSON.parse(ran.stdout.trim().split("\n").at(-1));
  assert.notEqual(threadId, main);
  assert.deepEqual(renamed.map(([, to]) => to), ["esm_7654321.js"]);
  assert.match(renamed[0][0], new RegExp(`^\\.writing-\\d+-${threadId}-[0-9a-f-]{36}$`, "u"));
  assert.equal(readFileSync(resolve(cache, "cd/esm_7654321.js"), "utf8"), "export const x = 1;");
  assert.deepEqual(readdirSync(cache).sort(), ["cd"]);
});
