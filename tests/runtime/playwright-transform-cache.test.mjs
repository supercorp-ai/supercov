import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readdirSync, readFileSync, realpathSync, rmSync } from "node:fs";
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
  assert.match(renamed[0][0], /^\.writing-\d+-0$/u);
  assert.equal(readFileSync(resolve(cache, "ab/config_1234567.js"), "utf8"), "export default {};");
  assert.equal(readFileSync(resolve(cache, "ab/by-descriptor"), "utf8"), "kept");
  assert.equal(readFileSync(resolve(root, "outside.txt"), "utf8"), "plain");
  assert.deepEqual(readdirSync(cache).sort(), ["ab"]);
});
