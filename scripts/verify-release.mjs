#!/usr/bin/env node
// Verify a published release on every channel, from the registries
// themselves: npm (the primary package, each native it pins, and a real
// `npx` run), PyPI (a wheel per native, and a real `uvx` run), RubyGems,
// crates.io's three crates, and the GitHub release's assets and notes.
//
// `publish.yml` succeeding is not the release being readable: npm's cache has
// lagged a native package by sixteen minutes after its publish job passed, and
// uvx right after a publish resolves an index that does not list it yet. Run
// this after the publish run, and again after a minute or two if a registry
// read says the version is missing.
//
// Usage: node scripts/verify-release.mjs [version]   (default: package.json's)
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const manifest = JSON.parse(readFileSync(resolve(repository, "package.json"), "utf8"));
const version = process.argv[2] ?? manifest.version;
const natives = Object.keys(manifest.optionalDependencies ?? {});
const crates = ["supercov-contracts", "supercov-engine", "supercov"];
// Every native but arm64 Windows, which Ruby has no platform for.
const gemsExpected = natives.length - 1;
const assetsExpected = 26;

let failed = 0;
const ok = (what) => console.log(`ok   ${what}`);
const fail = (what) => {
  failed += 1;
  console.log(`FAIL ${what}`);
};
const check = (passed, what, why = "") => (passed ? ok(what) : fail(why ? `${what}: ${why}` : what));

// Reads go to the registries with caches defeated; a tool's own cache is
// what made a slow registry look like a missing publish.
const json = async (url) => {
  try {
    const response = await fetch(url, {
      headers: { "user-agent": "supercov-release-check", "cache-control": "no-cache" },
    });
    return response.ok ? await response.json() : undefined;
  } catch {
    return undefined;
  }
};
const scratch = mkdtempSync(resolve(tmpdir(), "supercov-verify-"));
const run = (command, args, env = {}) =>
  spawnSync(command, args, { cwd: scratch, encoding: "utf8", env: { ...process.env, ...env } });
const reports = (result) => result.status === 0 && result.stdout.includes(version);

try {
  // npm: the primary package and each native list the version, and the
  // primary pins each native at it.
  const primary = await json("https://registry.npmjs.org/supercov");
  check(Boolean(primary?.versions?.[version]), `npm supercov lists ${version}`);
  for (const name of natives) {
    const document = await json(`https://registry.npmjs.org/${name}`);
    check(Boolean(document?.versions?.[version]), `npm ${name} lists ${version}`);
  }
  const pins = primary?.versions?.[version]?.optionalDependencies ?? {};
  const pinned = natives.filter((name) => pins[name] === version);
  check(pinned.length === natives.length, `npm supercov@${version} pins ${natives.length} natives`, `${pinned.length} pinned`);
  const npx = run("npx", ["-y", `supercov@${version}`, "--version"], {
    npm_config_cache: resolve(scratch, "npm"),
  });
  check(reports(npx), `npx supercov@${version} runs`, (npx.stdout || npx.stderr).trim().split("\n").at(-1));

  // PyPI: a wheel per native, and the version runs through uvx.
  const pypi = await json(`https://pypi.org/pypi/supercov-cli/${version}/json`);
  const wheels = (pypi?.urls ?? []).filter((file) => file.filename.endsWith(".whl")).length;
  check(wheels === natives.length, `PyPI supercov-cli ${version} has ${natives.length} wheels`, `${wheels} found`);
  const uvx = run("uvx", ["--refresh", "--from", `supercov-cli==${version}`, "supercov", "--version"], {
    UV_CACHE_DIR: resolve(scratch, "uv"),
  });
  check(reports(uvx), `uvx supercov-cli==${version} runs`, uvx.error ? "uvx is not installed" : (uvx.stdout || uvx.stderr).trim().split("\n").at(-1));

  // RubyGems: a gem per platform Ruby has.
  const gems = ((await json("https://rubygems.org/api/v1/versions/supercov.json")) ?? []).filter(
    (gem) => gem.number === version,
  ).length;
  check(gems >= gemsExpected, `RubyGems has ${gemsExpected} supercov ${version} gems`, `${gems} found`);

  // crates.io: the three crates, in their publish order.
  for (const crate of crates) {
    const found = await json(`https://crates.io/api/v1/crates/${crate}/${version}`);
    check(found?.version?.num === version, `crates.io ${crate} ${version}`);
  }

  // The GitHub release: every asset, and the notes it was published with.
  const release = run("gh", ["release", "view", `v${version}`, "--repo", "supercorp-ai/supercov", "--json", "assets,body"]);
  const published = release.status === 0 ? JSON.parse(release.stdout) : undefined;
  check(
    (published?.assets?.length ?? 0) >= assetsExpected,
    `GitHub release v${version} has ${assetsExpected} assets`,
    published ? `${published.assets.length} found` : (release.error ? "gh is not installed" : release.stderr.trim()),
  );
  check(/\*\*(Added|Fixed|Changed|Breaking)\*\*/.test(published?.body ?? ""), `GitHub release v${version} carries its notes`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

console.log(failed === 0 ? `[verify-release] ${version} is readable on every channel` : `[verify-release] ${failed} check(s) failed for ${version}`);
process.exitCode = failed === 0 ? 0 : 1;
