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

  run("python3", ["-m", "venv", resolve(temporary, "venv")]);
  const python = resolve(temporary, "venv", "bin", "python");
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
  const executable = resolve(temporary, "venv", "bin", "supercov");
  chmodSync(executable, 0o755);
  const covered = run(executable, ["--", process.execPath, "--test"], {
    cwd: project,
    env: environment,
  });
  assert.match(covered.stdout, /\[coverage\] evidence:/);
  console.log(`[pypi-wheel] supercov ${version} installed and completed a real run`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
