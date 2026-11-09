import assert from "node:assert/strict";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawn, spawnSync } from "node:child_process";
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

// A write that fails reaches Playwright as its own would, and leaves no
// partial file in the cache.
test("a cache write that fails leaves no partial file behind", t => {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-transform-cache-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const runtime = resolve(root, ".supercov/node_modules");
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), runtime, { recursive: true });
  const cache = resolve(root, "cache");
  const script = [
    "const fs = require('node:fs');",
    "const path = require('node:path');",
    // A folder where the compiled file goes: the rename onto it fails.
    "const target = path.join(process.env.PWTEST_CACHE_DIR, 'ef', 'busy_1111111.js');",
    "fs.mkdirSync(path.join(target, 'inside'), { recursive: true });",
    "try { fs.writeFileSync(target, 'export {};', 'utf8'); console.log('wrote'); }",
    "catch (error) { console.log(error.code); }",
  ].join("\n");
  const ran = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(runtime, "register.mjs")).href}`,
    "-e",
    script,
  ], { cwd: root, encoding: "utf8", timeout: 15000, env: { ...process.env, PWTEST_CACHE_DIR: cache } });
  assert.equal(ran.status, 0, ran.stdout + ran.stderr);
  assert.notEqual(ran.stdout.trim().split("\n").at(-1), "wrote");
  assert.deepEqual(readdirSync(cache).sort(), ["ef"]);
  assert.deepEqual(readdirSync(resolve(cache, "ef/busy_1111111.js")), ["inside"]);
});

// Before writing an entry, Playwright removes every cached file of that
// source -- the entry another process has just written for the same content
// among them -- and a process that read that entry's code reads its source
// map only when a test is declared. A sealed suite's containers start
// Playwright side by side over the run's one cache, and one test of a file
// now and then failed its `file:line` filter with "No tests found": its
// location came from the compiled lines. Supercov keeps every entry; the
// cache is the run's own.
//
// Two cold `playwright test file:line` processes over one cache, held in the
// order that loses the map: B misses and stops in its clear until A has
// written the entry; A stops after writing until B has cleared; B stops
// before writing its map until A has read the map. Plain Playwright loses
// it; under Supercov's preload both find their test.
test("a test is placed by its source map while another Playwright rewrites the entry", t => {
  const repository = resolve(import.meta.dirname, "../..");
  const cli = resolve(repository, "node_modules/playwright/cli.js");
  if (!existsSync(cli)) {
    t.skip("Playwright is not installed");
    return;
  }
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-transform-cache-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const runtime = resolve(root, ".supercov/node_modules");
  cpSync(resolve(repository, "runtime/javascript"), runtime, { recursive: true });
  const project = resolve(root, "project");
  mkdirSync(project);
  symlinkSync(resolve(repository, "node_modules"), resolve(project, "node_modules"), "junction");
  writeFileSync(resolve(project, "package.json"), '{ "private": true }\n');
  // Types the compiled file drops, so its lines differ from the source's.
  writeFileSync(resolve(project, "race.spec.ts"), [
    'import { test, expect } from "@playwright/test";',
    "type Pair = {",
    "  left: number;",
    "  right: number;",
    "};",
    "const sum = (pair: Pair): number => pair.left + pair.right;",
    'test("first", () => expect(sum({ left: 1, right: 1 })).toBe(2));',
    'test("second", () => expect(sum({ left: 2, right: 2 })).toBe(4));',
    "",
  ].join("\n"));
  // Holds one process at the points named above; each wait gives up after
  // twenty seconds rather than hang the suite, and the flags say how far
  // the order got.
  writeFileSync(resolve(root, "order.cjs"), `
const fs = require("node:fs");
const path = require("node:path");
const flags = process.env.RACE_FLAGS;
const role = process.env.RACE_ROLE;
const cache = path.resolve(process.env.PWTEST_CACHE_DIR) + path.sep;
const flag = (name) => fs.writeFileSync(path.join(flags, name), "");
const wait = (name) => {
  const until = Date.now() + 20000;
  while (!fs.existsSync(path.join(flags, name)) && Date.now() < until)
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20);
};
const inCache = (file) => typeof file === "string" && path.resolve(file).startsWith(cache);
const { readdirSync, writeFileSync, renameSync, readFileSync } = fs;
// A cache file arrives by a write in place, or, under Supercov, by a rename
// of the whole file onto its name.
const arriving = (target, before, after) => {
  fs.writeFileSync = function (file, ...rest) {
    if (inCache(file)) before(String(file));
    const result = writeFileSync.call(this, file, ...rest);
    if (inCache(file)) after(String(file));
    return result;
  };
  fs.renameSync = function (from, to, ...rest) {
    if (inCache(to)) before(String(to));
    const result = renameSync.call(this, from, to, ...rest);
    if (inCache(to)) after(String(to));
    return result;
  };
};
if (role === "B") {
  let cleared = false;
  fs.readdirSync = function (file, ...rest) {
    if (!cleared && inCache(file + path.sep)) {
      cleared = true;
      flag("b-missed");
      wait("a-wrote");
    }
    return readdirSync.call(this, file, ...rest);
  };
  arriving(".map", (file) => {
    if (file.endsWith(".map")) {
      flag("b-cleared");
      wait("a-read");
    }
  }, () => {});
}
if (role === "A") {
  arriving(".js", () => {}, (file) => {
    if (file.endsWith(".js")) {
      flag("a-wrote");
      wait("b-cleared");
    }
  });
  fs.readFileSync = function (file, ...rest) {
    try {
      return readFileSync.call(this, file, ...rest);
    } finally {
      if (inCache(file) && String(file).endsWith(".map")) flag("a-read");
    }
  };
}
`);
  const race = (label, preload) => {
    const cache = resolve(root, `cache-${label}`);
    const flags = resolve(root, `flags-${label}`);
    mkdirSync(flags);
    const options = (role) => ({
      cwd: project,
      encoding: "utf8",
      timeout: 120000,
      env: {
        ...process.env,
        PWTEST_CACHE_DIR: cache,
        RACE_FLAGS: flags,
        RACE_ROLE: role,
        NODE_OPTIONS: [
          ...(preload ? [`--import=${pathToFileURL(resolve(runtime, "register.mjs")).href}`] : []),
          `--require=${resolve(root, "order.cjs")}`,
        ].join(" "),
      },
    });
    const spawned = (role, line) =>
      new Promise((done) => {
        const child = spawn(process.execPath, [cli, "test", `race.spec.ts:${line}`, "--reporter=line", "--workers=1"], {
          ...options(role),
          stdio: "pipe",
        });
        let output = "";
        child.stdout.on("data", (chunk) => { output += chunk; });
        child.stderr.on("data", (chunk) => { output += chunk; });
        child.on("exit", (status) => done({ status, output }));
      });
    return (async () => {
      const b = spawned("B", 8);
      // A starts once B has missed and stopped, so both compile from cold.
      const until = Date.now() + 60000;
      while (!existsSync(resolve(flags, "b-missed")) && Date.now() < until)
        await new Promise((done) => setTimeout(done, 20));
      const a = spawned("A", 7);
      const [first, second] = await Promise.all([a, b]);
      return { a: first, b: second, reached: readdirSync(flags).sort() };
    })();
  };
  return (async () => {
    const plain = await race("plain");
    assert.deepEqual(plain.reached, ["a-read", "a-wrote", "b-cleared", "b-missed"], JSON.stringify(plain));
    assert.notEqual(plain.a.status, 0, plain.a.output);
    assert.match(plain.a.output, /No tests found/u);
    const kept = await race("supercov", true);
    assert.deepEqual(kept.reached, ["a-read", "a-wrote", "b-cleared", "b-missed"], JSON.stringify(kept));
    assert.equal(kept.a.status, 0, kept.a.output);
    assert.match(kept.a.output, /1 passed/u);
    assert.equal(kept.b.status, 0, kept.b.output);
    assert.match(kept.b.output, /1 passed/u);
  })();
});
