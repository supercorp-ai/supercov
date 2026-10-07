import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// A workspace as a run lays it out: the rewritten file where the project has
// it, what its author wrote kept beside the runtime, and a stand-in for the
// two Next.js processes that load its TypeScript setup.
function workspace(t) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-authored-view-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(root, ".supercov/node_modules"), { recursive: true });
  write("lib/client.ts", "instrumented\n");
  write(".supercov/node_modules/.authored/lib/client.ts", "authored\n");
  write(".supercov/node_modules/authored-sources.json", JSON.stringify(["lib/client.ts"]));
  write("node_modules/next/dist/lib/verify-typescript-setup.js",
    "module.exports = file => require('node:fs').readFileSync(file, 'utf8');\n");
  const entry = "process.stdout.write(require('../../lib/verify-typescript-setup.js')(process.argv[2]));\n";
  write("node_modules/next/dist/compiled/jest-worker/processChild.js", entry);
  write("node_modules/next/dist/bin/next", entry.replace("../../lib", "../lib"));
  return root;
}

function read(root, entrypoint) {
  const result = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(root, ".supercov/node_modules/register.mjs")).href}`,
    resolve(root, entrypoint),
    resolve(root, "lib/client.ts"),
  ], { cwd: root, encoding: "utf8", timeout: 15000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout;
}

test("Next's type-check worker reads each file as its author wrote it", t => {
  // A probe in a condition stops TypeScript narrowing through it, so the
  // function's inferred return type widened and `next build` failed on a
  // caller outside the source roots: "'supabase' is possibly 'null'".
  const root = workspace(t);
  assert.equal(read(root, "node_modules/next/dist/compiled/jest-worker/processChild.js"), "authored\n");
});

test("a Next process that also bundles keeps reading the instrumented copies", t => {
  // `next dev` sets TypeScript up in its main process, which serves the
  // application too: the authored view there would measure nothing.
  const root = workspace(t);
  assert.equal(read(root, "node_modules/next/dist/bin/next"), "instrumented\n");
});
