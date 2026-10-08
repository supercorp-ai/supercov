import assert from "node:assert/strict";
import { cpSync, mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "node:child_process";
import test from "node:test";

// A package without `"type"` whose TypeScript is compiled by a require hook,
// the way ts-node/register does it: `import` syntax, extensionless specifiers.
function project(t, manifest) {
  const root = realpathSync(mkdtempSync(resolve(tmpdir(), "supercov-require-hook-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (file, contents) => {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), contents);
  };
  cpSync(resolve(import.meta.dirname, "../../runtime/javascript"), resolve(root, ".supercov/node_modules"), { recursive: true });
  write("package.json", JSON.stringify(manifest));
  write("hook.cjs", [
    "const Module = require('node:module');",
    "const { readFileSync } = require('node:fs');",
    "Module._extensions['.ts'] = function compiled(module, filename) {",
    "  const source = readFileSync(filename, 'utf8')",
    "    .replace(/import (\\w+) from '([^']+)';/g, \"const $1 = require('$2').default;\")",
    "    .replace(/export default /g, 'exports.default = ')",
    "    .replace(/: number/g, '');",
    "  module._compile(source, filename);",
    "};",
    "",
  ].join("\n"));
  write("src/value.ts", "const value: number = 42;\nexport default value;\n");
  write("entry.ts", "import value from './src/value';\nprocess.stdout.write(String(value));\n");
  return root;
}

function run(root, preload) {
  return spawnSync(process.execPath, [
    "-r", resolve(root, "hook.cjs"),
    ...(preload ? [`--import=${pathToFileURL(resolve(root, ".supercov/node_modules/register.mjs")).href}`] : []),
    resolve(root, "entry.ts"),
  ], { cwd: root, encoding: "utf8", timeout: 20000 });
}

test("a TypeScript file still reaches the require hook that compiles it", t => {
  // Any `--import` makes Node load the entry point as an ES module, past the
  // hook: `node -r ts-node/register --test` passed alone and failed under
  // Supercov on its first extensionless import.
  const root = project(t, { name: "hooked", private: true });
  const alone = run(root, false);
  assert.equal(alone.stdout, "42", alone.stderr);
  const preloaded = run(root, true);
  assert.equal(preloaded.status, 0, preloaded.stdout + preloaded.stderr);
  assert.equal(preloaded.stdout, "42");
});
