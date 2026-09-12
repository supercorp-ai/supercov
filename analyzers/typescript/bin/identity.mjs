import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(import.meta.dirname, "..");
const sources = [
  "src/analyze.ts",
  "src/archive.ts",
  "src/compiler.ts",
  "src/types.ts",
  "src/pragmas.ts",
  "bin/query.mjs",
  "bin/identity.mjs",
  "tsconfig.json",
  // npm excludes package-lock.json from tarballs. Hash shipped build inputs,
  // not a checkout-only file, so installed and development checks are identical.
  "package.json",
];
const outputs = [
  "dist/analyze.js",
  "dist/archive.js",
  "dist/compiler.js",
  "dist/types.js",
  "dist/pragmas.js",
];
function digest(files) {
  const hash = createHash("sha256");
  for (const file of files)
    hash
      .update(file)
      .update("\0")
      .update(readFileSync(resolve(root, file)))
      .update("\0");
  return hash.digest("hex");
}
export function identity() {
  return {
    schema: 1,
    sourceSha256: digest(sources),
    compiledSha256: digest(outputs),
  };
}
export function checkedIdentity() {
  const built = JSON.parse(
    readFileSync(resolve(root, "dist/build-identity.json"), "utf8"),
  );
  const current = identity();
  if (JSON.stringify(built) !== JSON.stringify(current))
    throw new Error(
      "Assertion analyzer build is stale or modified; rebuild analyzers/typescript",
    );
  return current;
}
if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
)
  writeFileSync(
    resolve(root, "dist/build-identity.json"),
    JSON.stringify(identity()) + "\n",
  );
