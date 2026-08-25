import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-native-release-set-"));
const version = JSON.parse(readFileSync(resolve(repository, "package.json"), "utf8")).version;
const registry = JSON.parse(
  readFileSync(resolve(repository, "npm/native-targets.json"), "utf8"),
);
function digest(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}
function verify() {
  return spawnSync(
    process.execPath,
    [resolve(repository, "scripts/verify-native-release-set.mjs"), temporary],
    { cwd: repository, encoding: "utf8" },
  );
}

try {
  for (const [index, target] of registry.targets.entries()) {
    const tarballName = `${target.package}-${version}.tgz`;
    const binary = resolve(temporary, `binary-${index}`, target.executable);
    mkdirSync(resolve(binary, ".."), { recursive: true });
    const bytes = Buffer.from(`deterministic native binary ${index}\n`);
    writeFileSync(binary, bytes);
    if (target.platform !== "win32") chmodSync(binary, 0o755);
    const staged = spawnSync(
      process.execPath,
      [
        resolve(repository, "scripts/package-native.mjs"),
        "--target", target.rustTarget,
        "--binary", binary,
        "--out", resolve(temporary, "staged"),
      ],
      { cwd: repository, encoding: "utf8" },
    );
    assert.equal(staged.status, 0, staged.stderr || staged.stdout);
    const packed = spawnSync(
      "npm",
      [
        "pack",
        resolve(temporary, "staged", target.package),
        "--pack-destination", temporary,
        "--ignore-scripts",
        "--loglevel", "error",
      ],
      { cwd: repository, encoding: "utf8" },
    );
    assert.equal(packed.status, 0, packed.stderr || packed.stdout);
    const tarball = resolve(temporary, tarballName);
    writeFileSync(
      resolve(temporary, `${target.package}.checksums.json`),
      `${JSON.stringify({
        schemaVersion: 2,
        package: target.package,
        version,
        rustTarget: target.rustTarget,
        platform: target.platform,
        arch: target.arch,
        ...(target.libc ? { libc: target.libc } : {}),
        executable: target.executable,
        binary: {
          file: target.executable,
          bytes: bytes.byteLength,
          gzipBytes: 50 + index,
          sha256: digest(bytes),
        },
        npmTarball: {
          file: tarballName,
          bytes: statSync(tarball).size,
          sha256: digest(readFileSync(tarball)),
        },
      }, null, 2)}\n`,
    );
  }
  const valid = verify();
  assert.equal(valid.status, 0, valid.stderr || valid.stdout);
  const releaseSet = JSON.parse(readFileSync(resolve(temporary, "release-set.json"), "utf8"));
  assert.equal(releaseSet.version, version);
  assert.equal(releaseSet.packages.length, registry.targets.length);

  const damaged = resolve(temporary, `${registry.targets[0].package}-${version}.tgz`);
  writeFileSync(damaged, "damaged");
  const invalid = verify();
  assert.notEqual(invalid.status, 0);
  assert.match(invalid.stderr, /tarball size|tarball digest/);
  console.log("[native-release] complete-set and corruption gates passed");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
