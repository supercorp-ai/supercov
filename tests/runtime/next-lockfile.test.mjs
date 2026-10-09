import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// A project with a copy inside it, as a run lays it out, and Next's module
// that prints warnings, with its exports read through getters as Next's are.
function project(t) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-next-lockfile-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const copy = resolve(root, ".supercov/workspaces/workspace/app");
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(copy, file)), { recursive: true });
    writeFileSync(resolve(copy, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(copy, ".supercov/node_modules"), { recursive: true });
  write("node_modules/next/dist/build/output/log.js", [
    '"use strict";',
    "Object.defineProperty(exports, 'warnOnce', { enumerable: true, get: function() { return warnOnce; } });",
    "function warn(...message) { console.log(message.join(' ')); }",
    "function warnOnce(...message) { warn(...message); }",
    "",
    "//# sourceMappingURL=log.js.map",
    "",
  ].join("\n"));
  // What Next's lockfile search does with what it found, in both forms of
  // the warning: named last is the root it picked.
  const entry = [
    "const { warnOnce } = require(process.argv[3]);",
    "const lockFiles = JSON.parse(process.argv[2]);",
    "if (lockFiles.length > 1) warnOnce(`Warning: Next.js inferred your workspace root, but it may not be correct.\\n` +",
    "  ` We detected multiple lockfiles and selected the directory of ${lockFiles.at(-1)} as the root directory.\\n` +",
    "  ` To silence this warning, set \\`turbopack.root\\` in your Next.js config.\\n` +",
    "  ` Detected additional lockfiles: ${lockFiles.slice(0, -1).map((file) => `\\n   * ${file}`).join('')}\\n`);",
    "warnOnce('Warning: something else');",
    "",
  ].join("\n");
  write("node_modules/next/dist/bin/next", entry);
  write("server.js", entry);
  return { root, copy };
}

function printed(copy, entrypoint, lockfiles) {
  const result = spawnSync(process.execPath, [
    `--import=${pathToFileURL(resolve(copy, ".supercov/node_modules/register.mjs")).href}`,
    resolve(copy, entrypoint),
    JSON.stringify(lockfiles),
    resolve(copy, "node_modules/next/dist/build/output/log.js"),
  ], { cwd: copy, encoding: "utf8", timeout: 15000 });
  assert.equal(result.status, 0, result.stdout + result.stderr);
  return result.stdout;
}

test("Next does not warn about the copy's lockfile", t => {
  // Every build under Supercov warned twice that the workspace root "may not
  // be correct": the copy holds the project's lockfile, inside the project.
  const { root, copy } = project(t);
  const lockfiles = [resolve(copy, "package-lock.json"), resolve(root, "package-lock.json")];
  assert.equal(printed(copy, "node_modules/next/dist/bin/next", lockfiles), "Warning: something else\n");
});

test("it does not when the project starts Next from a server of its own", t => {
  // The warning was taken out only in a process Next's own command started.
  const { root, copy } = project(t);
  const lockfiles = [resolve(copy, "package-lock.json"), resolve(root, "package-lock.json")];
  assert.equal(printed(copy, "server.js", lockfiles), "Warning: something else\n");
});

test("a lockfile the project really has twice still warns, without the copy's", t => {
  const { root, copy } = project(t);
  const outer = resolve(dirname(root), "package-lock.json");
  const output = printed(copy, "node_modules/next/dist/bin/next", [
    resolve(copy, "package-lock.json"), resolve(root, "package-lock.json"), outer,
  ]);
  assert.match(output, /We detected multiple lockfiles and selected the directory of /);
  assert.ok(output.includes(`   * ${resolve(root, "package-lock.json")}\n`), output);
  assert.ok(!output.includes(".supercov/workspaces"), output);
  assert.match(output, /Warning: something else\n$/);
});
