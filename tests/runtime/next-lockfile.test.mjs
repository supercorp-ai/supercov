import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// A project with a copy inside it, as a run lays it out, and Next's own
// function for the warning: it reports what it was given beyond the root.
function project(t) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-next-lockfile-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const copy = resolve(root, ".supercov/workspaces/workspace/app");
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(copy, file)), { recursive: true });
    writeFileSync(resolve(copy, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(copy, ".supercov/node_modules"), { recursive: true });
  write("node_modules/next/dist/lib/find-root.js",
    "function warnDuplicatedLockFiles(lockFiles) {\n    if (lockFiles.length > 1) console.log(lockFiles.slice(0, -1).join('\\n'));\n}\nmodule.exports = { warnDuplicatedLockFiles };\n");
  const entry = "require('../lib/find-root.js').warnDuplicatedLockFiles(JSON.parse(process.argv[2]));\n";
  write("node_modules/next/dist/bin/next", entry);
  write("node_modules/other/dist/bin/tool", entry.replace("../lib", "../../../next/dist/lib"));
  return { root, copy };
}

function warned(copy, entrypoint, lockfiles) {
  const result = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(copy, ".supercov/node_modules/register.mjs")).href}`,
    resolve(copy, entrypoint),
    JSON.stringify(lockfiles),
  ], { cwd: copy, encoding: "utf8", timeout: 15000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout.trim().split("\n").filter(Boolean);
}

test("Next does not count the copy's lockfile as a second one", t => {
  // Every build under Supercov warned twice that the workspace root "may not
  // be correct": the copy holds the project's lockfile, inside the project.
  const { root, copy } = project(t);
  const lockfiles = [resolve(copy, "package-lock.json"), resolve(root, "package-lock.json")];
  assert.deepEqual(warned(copy, "node_modules/next/dist/bin/next", lockfiles), []);
});

test("a lockfile the project really has twice still warns", t => {
  const { root, copy } = project(t);
  const outer = resolve(dirname(root), "package-lock.json");
  const lockfiles = [resolve(copy, "package-lock.json"), resolve(root, "package-lock.json"), outer];
  assert.deepEqual(warned(copy, "node_modules/next/dist/bin/next", lockfiles), [resolve(root, "package-lock.json")]);
});

test("another program reading Next's function sees it unchanged", t => {
  const { root, copy } = project(t);
  const lockfiles = [resolve(copy, "package-lock.json"), resolve(root, "package-lock.json")];
  assert.deepEqual(warned(copy, "node_modules/other/dist/bin/tool", lockfiles), [resolve(copy, "package-lock.json")]);
});
