import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { tmpdir } from "node:os";
import {
  appendFileSync,
  cpSync,
  mkdtempSync,
  mkdirSync,
  rmSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import { compilerIdentityFromEntry } from "../bin/compiler-identity.mjs";

test("native compiler identity includes client code, the executable and standard libraries", (t) => {
  const require = createRequire(new URL("../package.json", import.meta.url));
  const compiler = dirname(require.resolve("typescript-native/package.json"));
  const platform = `@typescript/typescript-${process.platform}-${process.arch}`;
  const native = dirname(require.resolve(`${platform}/package.json`));
  const root = mkdtempSync(resolve(tmpdir(), "supercov-compiler-identity-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const compilerCopy = resolve(root, "node_modules/typescript");
  const nativeCopy = resolve(root, "node_modules", platform);
  mkdirSync(resolve(root, "node_modules/@typescript"), { recursive: true });
  cpSync(compiler, compilerCopy, { recursive: true });
  cpSync(native, nativeCopy, { recursive: true });
  const entry = resolve(compilerCopy, "lib/version.cjs");
  const first = compilerIdentityFromEntry(entry);
  assert.equal(first.backend, "typescript-native-7");
  appendFileSync(
    resolve(compilerCopy, "dist/api/sync/api.js"),
    "\n// identity probe\n",
  );
  const clientChanged = compilerIdentityFromEntry(entry);
  assert.notEqual(clientChanged.sha256, first.sha256);
  assert.equal(clientChanged.nativeSha256, first.nativeSha256);
  // This copy is only hashed, never executed after modification.
  appendFileSync(
    resolve(
      nativeCopy,
      "lib",
      process.platform === "win32" ? "tsc.exe" : "tsc",
    ),
    Buffer.from([0]),
  );
  const binaryChanged = compilerIdentityFromEntry(entry);
  assert.notEqual(binaryChanged.nativeSha256, first.nativeSha256);
  appendFileSync(
    resolve(nativeCopy, "lib/lib.es5.d.ts"),
    "\n// identity probe\n",
  );
  assert.notEqual(
    compilerIdentityFromEntry(entry).sha256,
    binaryChanged.sha256,
  );
  const metadataPath = resolve(nativeCopy, "package.json");
  const metadata = JSON.parse(readFileSync(metadataPath, "utf8"));
  writeFileSync(
    metadataPath,
    JSON.stringify({ ...metadata, version: "7.0.3" }),
  );
  assert.throws(
    () => compilerIdentityFromEntry(entry),
    /native package identity mismatch/,
  );
});
