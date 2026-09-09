// A native Node probe: no Rust build or test instrumentation is needed.
import assert from "node:assert/strict";
import { cpSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { resolve, toNamespacedPath } from "node:path";
import { pathToFileURL } from "node:url";

const temporary = mkdtempSync(resolve(tmpdir(), "supercov-compiler-probe-"));
try {
  mkdirSync(resolve(temporary, "node_modules"));
  cpSync(
    resolve(
      import.meta.dirname,
      "../analyzers/typescript/node_modules/typescript",
    ),
    resolve(temporary, "node_modules/typescript"),
    { recursive: true },
  );
  const packageFile = resolve(temporary, "package.json");
  writeFileSync(
    packageFile,
    JSON.stringify({ name: "compiler-probe", type: "module" }),
  );
  const forms = {
    ordinary: packageFile,
    namespaced: toNamespacedPath(packageFile),
    namespacedFileUrl: pathToFileURL(toNamespacedPath(packageFile)),
  };
  const results = Object.entries(forms).map(([name, file]) => {
    try {
      const require = createRequire(file);
      const entry = require.resolve("typescript");
      return {
        name,
        file: String(file),
        entry,
        version: require(entry).version,
      };
    } catch (error) {
      return {
        name,
        file: String(file),
        error: { code: error.code, message: error.message, stack: error.stack },
      };
    }
  });
  console.log(
    JSON.stringify(
      { platform: process.platform, node: process.version, results },
      null,
      2,
    ),
  );
  assert.equal(results[0].version, "5.8.3");
  assert.equal(
    results[2].version,
    "5.8.3",
    "file-URL compiler lookup must work",
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
