#!/usr/bin/env node

import assert from "node:assert/strict";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const repository = resolve(import.meta.dirname, "..");
const version = JSON.parse(readFileSync(resolve(repository, "package.json"))).version;
const executableName = process.platform === "win32" ? "supercov.exe" : "supercov";
const binary = resolve(
  process.env.SUPERCOV_RELEASE_BINARY ??
    resolve(repository, "target", "release", executableName),
);

function localGemPlatform() {
  if (process.platform === "darwin" && process.arch === "arm64") return "arm64-darwin";
  if (process.platform === "darwin" && process.arch === "x64") return "x86_64-darwin";
  throw new Error(
    "set SUPERCOV_GEM_PLATFORM when packaging outside macOS arm64/x64",
  );
}

function rubyString(value) {
  return JSON.stringify(value);
}

const platform = process.env.SUPERCOV_GEM_PLATFORM ?? localGemPlatform();
const outputRoot = resolve(repository, "target", "rubygems");
const staging = resolve(outputRoot, `supercov-${version}-${platform}`);
const output = resolve(outputRoot, `supercov-${version}-${platform}.gem`);

rmSync(staging, { recursive: true, force: true, maxRetries: 10, retryDelay: 20 });
mkdirSync(resolve(staging, "exe"), { recursive: true });
mkdirSync(resolve(staging, "lib"), { recursive: true });
mkdirSync(resolve(staging, "libexec"), { recursive: true });
copyFileSync(resolve(repository, "packaging", "rubygems", "exe", "supercov"), resolve(staging, "exe", "supercov"));
copyFileSync(binary, resolve(staging, "libexec", executableName));
copyFileSync(resolve(repository, "LICENSE"), resolve(staging, "LICENSE"));
copyFileSync(resolve(repository, "README.md"), resolve(staging, "README.md"));
if (executableName !== "supercov") {
  throw new Error("the Ruby wrapper currently requires a packaged executable named supercov");
}
chmodSync(resolve(staging, "exe", "supercov"), 0o755);
chmodSync(resolve(staging, "libexec", "supercov"), 0o755);
writeFileSync(
  resolve(staging, "lib", "supercov.rb"),
  `# frozen_string_literal: true\n\nmodule Supercov\n  VERSION = ${rubyString(version)}\nend\n`,
);

const gemspec = `Gem::Specification.new do |spec|
  spec.name = "supercov"
  spec.version = ${rubyString(version)}
  spec.platform = Gem::Platform.new(${rubyString(platform)})
  spec.summary = "Zero-configuration, runner-aware structural and MC/DC coverage"
  spec.description = "The platform-native Supercov CLI, powered by the single Rust engine."
  spec.authors = ["Supercorp"]
  spec.email = ["hello@supercorp.ai"]
  spec.homepage = "https://github.com/supercorp-ai/supercov"
  spec.license = "MIT"
  spec.required_ruby_version = Gem::Requirement.new(">= 2.6")
  spec.files = ["LICENSE", "README.md", "exe/supercov", "lib/supercov.rb", "libexec/supercov"]
  spec.bindir = "exe"
  spec.executables = ["supercov"]
  spec.require_paths = ["lib"]
  spec.metadata = {
    "bug_tracker_uri" => "https://github.com/supercorp-ai/supercov/issues",
    "source_code_uri" => "https://github.com/supercorp-ai/supercov",
  }
end
`;
writeFileSync(resolve(staging, "supercov.gemspec"), gemspec);

mkdirSync(outputRoot, { recursive: true });
const built = spawnSync(
  "gem",
  ["build", "supercov.gemspec", "--output", output],
  { cwd: staging, encoding: "utf8" },
);
assert.equal(built.status, 0, built.stderr || built.stdout);
console.log(`[rubygem] built ${output}`);
