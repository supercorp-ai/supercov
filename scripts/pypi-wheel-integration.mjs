#!/usr/bin/env node

import assert from "node:assert/strict";
import {
  chmodSync,
  cpSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const version = JSON.parse(readFileSync(resolve(repository, "package.json"))).version;
const wheelDistribution = "supercov_cli";
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-pypi-wheel-"));

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: "utf8", ...options });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result;
}

try {
  const wheelDirectory = resolve(repository, "target", "wheels");
  const wheels = readdirSync(wheelDirectory).filter(
    (entry) => entry.startsWith(`${wheelDistribution}-${version}-`) && entry.endsWith(".whl"),
  );
  assert.equal(
    wheels.length,
    1,
    `expected one supercov-cli ${version} wheel, found ${wheels}`,
  );

  const environment = { ...process.env };
  delete environment.SUPERCOV_RUST_BINARY;

  // The wheel requires CPython 3.12 or newer, which `python3` need not be on a
  // developer machine; SUPERCOV_PYTHON names the interpreter to use. A venv
  // puts its executables under Scripts/ on Windows and bin/ everywhere else.
  run(process.env.SUPERCOV_PYTHON ?? "python3", ["-m", "venv", resolve(temporary, "venv")]);
  const windows = process.platform === "win32";
  const scripts = resolve(temporary, "venv", windows ? "Scripts" : "bin");
  const python = resolve(scripts, windows ? "python.exe" : "python");
  run(python, [
    "-m",
    "pip",
    "install",
    "--disable-pip-version-check",
    resolve(wheelDirectory, wheels[0]),
  ]);

  const project = resolve(temporary, "project");
  cpSync(resolve(repository, "tests/fixtures/no-build-node"), project, {
    recursive: true,
  });
  const executable = resolve(scripts, windows ? "supercov.exe" : "supercov");
  if (!windows) chmodSync(executable, 0o755);
  const covered = run(executable, ["--", process.execPath, "--test"], {
    cwd: project,
    env: environment,
  });
  assert.match(covered.stdout, /\[coverage\] evidence:/);
  console.log(`[pypi-wheel] supercov ${version} installed and completed a real run`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
