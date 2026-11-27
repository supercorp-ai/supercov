import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

import { loadSync } from "../../runtime/javascript/resolve-loader.mjs";

// A server's runtime rescans the CommonJS module cache for modules it has not
// seen only when the load hook's count moved. Built-ins never enter that cache,
// so they must not move the count; every other load must.
test("only a load that can add to the module cache moves the count", () => {
  const directory = mkdtempSync(resolve(tmpdir(), "supercov-loads-"));
  const file = resolve(directory, "a.cjs");
  writeFileSync(file, "module.exports = 1;\n");
  const require = createRequire(import.meta.url);
  require("node:path");
  require("node:fs");
  assert.ok(
    Object.keys(require.cache).every((key) => !key.startsWith("node:") && key !== "path" && key !== "fs"),
    "a built-in is never an entry of the CommonJS cache",
  );
  const next = (url) => ({ format: url.startsWith("node:") ? "builtin" : "commonjs", source: url.startsWith("node:") ? null : "module.exports = 1;\n", shortCircuit: true });
  const require_ = { conditions: ["require"] };
  delete globalThis.__SUPERCOV_MODULE_LOADS__;
  loadSync("node:path", require_, next);
  loadSync("node:fs", require_, next);
  assert.equal(globalThis.__SUPERCOV_MODULE_LOADS__, undefined);
  loadSync(pathToFileURL(file).href, require_, next);
  assert.equal(globalThis.__SUPERCOV_MODULE_LOADS__, 1);
  loadSync(pathToFileURL(file).href, { conditions: ["import"] }, next);
  assert.equal(globalThis.__SUPERCOV_MODULE_LOADS__, 2);
  loadSync("node:url", require_, next);
  assert.equal(globalThis.__SUPERCOV_MODULE_LOADS__, 2);
  delete globalThis.__SUPERCOV_MODULE_LOADS__;
});
