#!/usr/bin/env node

import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const version = JSON.parse(readFileSync(resolve(repository, "package.json"))).version;
const temporary = mkdtempSync(resolve(tmpdir(), "supercov-rubygem-"));

// `Gem::Platform.local` is versioned -- "arm64-darwin-22" -- while a published
// platform is the family, "arm64-darwin". A gem matches when the local platform
// starts with the family, which is what RubyGems' own resolver decides.
function Gem_matches(platform, localPlatform) {
  return localPlatform === platform || localPlatform.startsWith(`${platform}-`);
}

function run(program, args, options = {}) {
  const result = spawnSync(program, args, { encoding: "utf8", ...options });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return result;
}

try {
  const gemDirectory = resolve(repository, "target", "rubygems");
  const built = readdirSync(gemDirectory).filter(
    (entry) => entry.startsWith(`supercov-${version}-`) && entry.endsWith(".gem"),
  );
  // A release builds one gem per platform into this directory, and only the
  // one for this machine can be installed here. Ask Ruby which platform it is
  // rather than mapping Node's names onto RubyGems' own.
  const localPlatform = run("ruby", ["-e", "print Gem::Platform.local.to_s"]).stdout.trim();
  const registry = JSON.parse(
    readFileSync(resolve(repository, "npm/native-targets.json"), "utf8"),
  );
  const platforms = registry.targets
    .map((target) => target.gemPlatform)
    .filter(Boolean)
    .filter((platform) => Gem_matches(platform, localPlatform));
  const gems = built.filter((entry) =>
    platforms.some((platform) => entry === `supercov-${version}-${platform}.gem`),
  );
  assert.equal(
    gems.length,
    1,
    `expected exactly one supercov ${version} gem installable on ${localPlatform}, found ${gems} among ${built}`,
  );

  const gemHome = resolve(temporary, "gem-home");
  const bindir = resolve(temporary, "bin");
  // On Windows both `gem` and the installed shim are batch files, which only a
  // command interpreter can start.
  const windows = process.platform === "win32";
  const shell = (command, args) =>
    windows
      ? [process.env.ComSpec ?? "cmd.exe", ["/d", "/s", "/c", command, ...args]]
      : [command, args];
  run(...shell(windows ? "gem.cmd" : "gem", [
    "install",
    "--local",
    "--no-document",
    "--install-dir",
    gemHome,
    "--bindir",
    bindir,
    resolve(gemDirectory, gems[0]),
  ]));

  const project = resolve(temporary, "project");
  cpSync(resolve(repository, "tests/fixtures/no-build-node"), project, {
    recursive: true,
    filter: (source) => !source.endsWith("/.supercov"),
  });
  const covered = run(...shell(resolve(bindir, windows ? "supercov.bat" : "supercov"), ["--", process.execPath, "--test"]), {
    cwd: project,
    env: { ...process.env, GEM_HOME: gemHome, GEM_PATH: gemHome },
  });
  assert.match(covered.stdout, /\[coverage\] evidence:/);
  console.log(`[rubygem] supercov ${version} installed and completed a real run`);
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
}
