import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

function option(name) {
  const index = process.argv.indexOf(name);
  return index === -1 ? undefined : process.argv[index + 1];
}

const rustTarget = option("--target");
const binary = option("--binary");
const tarball = option("--tarball");
const output = option("--out");
if (!rustTarget || !binary || !tarball || !output) {
  throw new Error("usage: native-artifact-check.mjs --target <target> --binary <path> --tarball <path> --out <json>");
}
assert(statSync(binary).isFile(), `binary is not a regular file: ${binary}`);
assert(statSync(tarball).isFile(), `tarball is not a regular file: ${tarball}`);
const help = spawnSync(resolve(binary), ["help"], { encoding: "utf8" });
assert.equal(help.status, 0, help.stderr || help.stdout);
const binaryBytes = readFileSync(binary);
const compressedBytes = gzipSync(binaryBytes, { level: 9 }).byteLength;
const compressedGate = 15 * 1024 * 1024;
assert(
  compressedBytes <= compressedGate,
  `${rustTarget} compressed binary is ${compressedBytes} bytes, above the ${compressedGate}-byte gate`,
);
function digest(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}
const result = {
  schemaVersion: 1,
  rustTarget,
  binary: {
    file: resolve(binary),
    bytes: binaryBytes.byteLength,
    gzipBytes: compressedBytes,
    sha256: digest(binary),
  },
  npmTarball: {
    file: resolve(tarball),
    bytes: statSync(tarball).size,
    sha256: digest(tarball),
  },
};
writeFileSync(output, `${JSON.stringify(result, null, 2)}\n`);
console.log(
  `[native-artifact] ${rustTarget} binary=${result.binary.bytes} gzip=${compressedBytes} npm=${result.npmTarball.bytes}`,
);
