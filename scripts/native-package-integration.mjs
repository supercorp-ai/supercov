import assert from "node:assert/strict";
import {
  chmodSync,
  cpSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { nativePackageFor } from "../bin/native.js";

const repository = resolve(import.meta.dirname, "..");
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-native-package-"));

function option(name) {
  const index = process.argv.indexOf(name);
  return index === -1 ? undefined : process.argv[index + 1];
}

function run(program, arguments_, options = {}) {
  const result = spawnSync(program, arguments_, { encoding: "utf8", ...options });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result.stdout.trim();
}

try {
  const targetRegistry = JSON.parse(
    readFileSync(resolve(repository, "npm/native-targets.json"), "utf8"),
  );
  const selected = nativePackageFor();
  const requestedTarget = option("--target");
  const target = requestedTarget
    ? targetRegistry.targets.find(entry => entry.rustTarget === requestedTarget)
    : targetRegistry.targets.find(entry => entry.package === selected.packageName);
  assert(target, `runtime loader target ${selected.packageName} is absent from native-targets.json`);
  assert.equal(
    target.package,
    selected.packageName,
    `packed-install test must run on its native host (${target.package} requested, ${selected.packageName} selected)`,
  );
  for (const entry of targetRegistry.targets) {
    assert.deepEqual(
      nativePackageFor(entry.platform, entry.arch, entry.libc),
      { packageName: entry.package, executable: entry.executable },
    );
  }
  assert.throws(
    () => nativePackageFor("freebsd", "x64"),
    /no native Supercov binary is published/,
  );
  const packageRoot = run(process.execPath, [
    resolve(repository, "scripts/package-native.mjs"),
    "--target", target.rustTarget,
    "--binary", option("--binary") ?? resolve(repository, `target/release/${target.executable}`),
    "--out", resolve(temporary, "platform"),
  ]);
  const platformPack = JSON.parse(
    run("npm", ["pack", "--ignore-scripts", "--json"], { cwd: packageRoot }),
  )[0].filename;

  const mainRoot = resolve(temporary, "main");
  cpSync(resolve(repository, "bin"), resolve(mainRoot, "bin"), { recursive: true });
  cpSync(resolve(repository, "dist"), resolve(mainRoot, "dist"), { recursive: true });
  for (const file of ["package.json", "README.md", "LICENSE"])
    cpSync(resolve(repository, file), resolve(mainRoot, file));
  const mainPack = JSON.parse(
    run("npm", ["pack", "--ignore-scripts", "--json"], { cwd: mainRoot }),
  )[0].filename;

  const consumer = resolve(temporary, "consumer");
  cpSync(resolve(repository, "tests/fixtures/no-build-node"), consumer, { recursive: true });
  const consumerPackage = JSON.parse(readFileSync(resolve(consumer, "package.json"), "utf8"));
  consumerPackage.dependencies = {
    supercov: `file:${resolve(mainRoot, mainPack)}`,
    [target.package]: `file:${resolve(packageRoot, platformPack)}`,
  };
  writeFileSync(resolve(consumer, "package.json"), `${JSON.stringify(consumerPackage, null, 2)}\n`);
  run("npm", ["install", "--ignore-scripts"], { cwd: consumer });
  const executable = resolve(consumer, "node_modules/.bin/supercov");
  if (process.platform !== "win32") chmodSync(executable, 0o755);
  const covered = spawnSync(executable, ["--", process.execPath, "--test"], {
    cwd: consumer,
    encoding: "utf8",
    env: { ...process.env, SUPERCOV_ENGINE: "rust" },
  });
  assert.equal(covered.status, 0, covered.stderr || covered.stdout);
  assert.match(covered.stdout, /\[coverage\] evidence:/);

  const installedPackage = resolve(consumer, "node_modules", target.package);
  const installedManifest = resolve(installedPackage, "package.json");
  const validManifest = readFileSync(installedManifest, "utf8");
  const wrongVersion = JSON.parse(validManifest);
  wrongVersion.version = "0.0.0-invalid";
  writeFileSync(installedManifest, `${JSON.stringify(wrongVersion, null, 2)}\n`);
  const mismatched = spawnSync(executable, ["help"], {
    cwd: consumer,
    encoding: "utf8",
    env: { ...process.env, SUPERCOV_ENGINE: "rust" },
  });
  assert.equal(mismatched.status, 1);
  assert.match(mismatched.stderr, /native package version mismatch/);
  writeFileSync(installedManifest, validManifest);

  const installedBinary = resolve(installedPackage, "bin", target.executable);
  unlinkSync(installedBinary);
  const missingBinary = spawnSync(executable, ["help"], {
    cwd: consumer,
    encoding: "utf8",
    env: { ...process.env, SUPERCOV_ENGINE: "rust" },
  });
  assert.equal(missingBinary.status, 1);
  assert.match(missingBinary.stderr, /does not contain a regular Supercov executable/);
  cpSync(resolve(packageRoot, "bin", target.executable), installedBinary);
  if (process.platform !== "win32") chmodSync(installedBinary, 0o755);

  const hiddenPackage = `${installedPackage}.missing`;
  renameSync(installedPackage, hiddenPackage);
  const missingPackage = spawnSync(executable, ["help"], {
    cwd: consumer,
    encoding: "utf8",
    env: { ...process.env, SUPERCOV_ENGINE: "rust" },
  });
  assert.equal(missingPackage.status, 1);
  assert.match(missingPackage.stderr, /optional native package .* is missing/);
  renameSync(hiddenPackage, installedPackage);
  console.log(`[native-package] packed install and execution passed with ${target.package}`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
