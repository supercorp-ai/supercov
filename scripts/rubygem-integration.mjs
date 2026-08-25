#!/usr/bin/env node

import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const version = JSON.parse(readFileSync(resolve(repository, "package.json"))).version;
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-rubygem-"));

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: "utf8", ...options });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result;
}

try {
  const gemDirectory = resolve(repository, "target", "rubygems");
  const gems = readdirSync(gemDirectory).filter(
    (entry) => entry.startsWith(`supercov-${version}-`) && entry.endsWith(".gem"),
  );
  assert.equal(gems.length, 1, `expected one supercov ${version} gem, found ${gems}`);

  const gemHome = resolve(temporary, "gem-home");
  const bindir = resolve(temporary, "bin");
  run("gem", [
    "install",
    "--local",
    "--no-document",
    "--install-dir",
    gemHome,
    "--bindir",
    bindir,
    resolve(gemDirectory, gems[0]),
  ]);

  const project = resolve(temporary, "project");
  cpSync(resolve(repository, "tests/fixtures/no-build-node"), project, {
    recursive: true,
    filter: (source) => !source.endsWith("/.supercov"),
  });
  const covered = run(resolve(bindir, "supercov"), ["--", process.execPath, "--test"], {
    cwd: project,
    env: { ...process.env, GEM_HOME: gemHome, GEM_PATH: gemHome },
  });
  assert.match(covered.stdout, /\[coverage\] evidence:/);
  console.log(`[rubygem] supercov ${version} installed and completed a real run`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
