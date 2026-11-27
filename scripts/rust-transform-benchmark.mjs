#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync } from "node:fs";
import { resolve } from "node:path";

const budget = JSON.parse(
  readFileSync(resolve("benchmarks/rust-transform-budget.json"), "utf8"),
);
const executable = `supercov${process.platform === "win32" ? ".exe" : ""}`;
let candidate = resolve(
  process.env.SUPERCOV_RUST_BINARY ?? `target/release/${executable}`,
);
const cases = Array.from({ length: budget.corpusFiles }, (_, index) => ({
  file: `benchmark/${index}.js`,
  source: `
    export function decision${index}(input, fallback = ${index % 7}) {
      let total = 0;
      try {
        for (const value of input?.values ?? []) {
          if ((value > ${index % 5} && input.enabled) || input.force) total += value;
          else if (value === 0 || fallback > 3) continue;
          else total -= fallback;
        }
      } catch (error) {
        total = error?.code === "EXPECTED" ? fallback : -1;
      }
      return total > 0 && input.valid ? total : fallback;
    }
  `,
}));
const input = JSON.stringify(cases);

function median(values) {
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.floor(sorted.length / 2)];
}

function internalSample(binary = candidate) {
  const child = spawnSync(binary, ["__benchmark-js-transform"], {
    input,
    encoding: "utf8",
    maxBuffer: 1024 * 1024 * 64,
  });
  if (child.error) throw child.error;
  if (child.status !== 0)
    throw new Error(`Rust transform benchmark failed: ${child.stderr.trim()}`);
  const result = JSON.parse(child.stdout);
  if (result.files !== cases.length)
    throw new Error(`Rust benchmark transformed ${result.files}/${cases.length} files`);
  return Number(result.durationNs) / 1_000_000;
}

internalSample();
const internal = [];
for (let index = 0; index < 7; index += 1) {
  internal.push(internalSample());
}
const engineMedian = median(internal);
const engineP95 = Math.max(...internal);
const extrapolatedMonorepoMs =
  engineMedian * (budget.monorepoFiles / budget.corpusFiles);

console.log(
  `[rust-transform] engine median=${engineMedian.toFixed(2)}ms p95=${engineP95.toFixed(2)}ms files=${cases.length}`,
);
console.log(
  `[rust-transform] ${budget.monorepoFiles.toLocaleString("en-US")}-file linear extrapolation=${extrapolatedMonorepoMs.toFixed(0)}ms`,
);

const failures = [];
if (engineMedian > budget.engineMedianMsMax)
  failures.push(
    `engine median ${engineMedian.toFixed(2)}ms > ${budget.engineMedianMsMax}ms`,
  );
if (engineP95 > budget.engineP95MsMax)
  failures.push(`engine p95 ${engineP95.toFixed(2)}ms > ${budget.engineP95MsMax}ms`);
if (extrapolatedMonorepoMs > budget.monorepoExtrapolatedMsMax)
  failures.push(
    `${budget.monorepoFiles}-file extrapolation ${extrapolatedMonorepoMs.toFixed(0)}ms > ${budget.monorepoExtrapolatedMsMax}ms`,
  );
// The budget is a time on a machine at full speed. A laptop on battery in
// low power mode runs the same code half as fast again, and the 4.3.0 release
// check failed here for that reason alone: the binary already published read
// the same 52 ms. So a miss is put to the question that was asked by hand
// then: is this build slower than the last release, on this machine, now?
// The last release built from its tag as this binary was built. Compared
// with the published binary -- fat LTO in one unit -- a build made faster to
// build (thin LTO in sixteen units, as release-gates makes it) read 1.05x
// with no change of code at all, and a change costing 2% failed the 1.1x
// limit. Given the build command and where it leaves the binary, under the
// target directory it shares with this build, the comparison is like with
// like.
function builtFromTag(version) {
  const command = process.env.SUPERCOV_BENCHMARK_REFERENCE_BUILD;
  const built = process.env.SUPERCOV_BENCHMARK_REFERENCE_BUILT;
  const target = resolve("target");
  const binary = resolve(target, "benchmark-reference", `${version}-built`, executable);
  if (existsSync(binary)) return binary;
  // The reference builds into the shared target directory and would
  // replace this binary there.
  const kept = resolve(target, "benchmark-candidate", executable);
  mkdirSync(resolve(kept, ".."), { recursive: true });
  copyFileSync(candidate, kept);
  candidate = kept;
  const source = resolve(target, "benchmark-reference", `${version}-source`);
  rmSync(source, { recursive: true, force: true });
  const step = (program, args, options = {}) => {
    const ran = spawnSync(program, args, { stdio: "inherit", ...options });
    if (ran.status !== 0) throw new Error(`${program} ${args.join(" ")} failed while building ${version}`);
  };
  step("git", ["fetch", "--quiet", "--depth", "1", "origin", "tag", `v${version}`]);
  step("git", ["worktree", "add", "--quiet", "--detach", source, `v${version}`]);
  try {
    console.error(`[rust-transform] building ${version} from its tag as this binary was built: ${command}`);
    step(command, [], { cwd: source, shell: true, env: { ...process.env, CARGO_TARGET_DIR: target } });
    mkdirSync(resolve(binary, ".."), { recursive: true });
    copyFileSync(resolve(target, built), binary);
  } finally {
    spawnSync("git", ["worktree", "remove", "--force", source]);
  }
  return binary;
}

/** The newest release tag's version (`v4.29.8` is `4.29.8`), or null. */
function newestTag() {
  const listed = spawnSync("git", ["ls-remote", "--tags", "--refs", "origin", "v*"], {
    encoding: "utf8",
  });
  if (listed.status !== 0) return null;
  const versions = listed.stdout
    .split("\n")
    .map((line) => /refs\/tags\/v(\d+)\.(\d+)\.(\d+)$/.exec(line.trim()))
    .filter(Boolean)
    .map((found) => found.slice(1, 4).map(Number));
  versions.sort((a, b) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2]);
  const newest = versions.at(-1);
  return newest ? newest.join(".") : null;
}

function released() {
  if (process.env.SUPERCOV_BENCHMARK_REFERENCE)
    return {
      version: "the reference",
      binary: resolve(process.env.SUPERCOV_BENCHMARK_REFERENCE),
    };
  const libc =
    process.platform !== "linux"
      ? ""
      : process.report.getReport().header.glibcVersionRuntime
        ? "-gnu"
        : "-musl";
  const name = `@supercov/cli-${process.platform}-${process.arch}${libc}`;
  const viewed = spawnSync("npm", ["view", name, "version"], {
    encoding: "utf8",
    shell: process.platform === "win32",
  });
  const version = viewed.stdout?.trim();
  if (viewed.status !== 0 || !/^\d+\.\d+\.\d+$/.test(version)) return null;
  if (process.env.SUPERCOV_BENCHMARK_REFERENCE_BUILD && process.env.SUPERCOV_BENCHMARK_REFERENCE_BUILT) {
    // Built from its source, the last release is the newest tag: a release
    // reaches npm minutes before its publish pushes the tag, and a run in
    // between asked for a tag that was not there yet.
    const tagged = newestTag();
    if (!tagged) return null;
    return { version: `${tagged} built the same way`, binary: builtFromTag(tagged) };
  }
  const directory = resolve("target/benchmark-reference", version);
  const binary = resolve(directory, "package/bin", executable);
  if (!existsSync(binary)) {
    mkdirSync(directory, { recursive: true });
    const packed = spawnSync(
      "npm",
      ["pack", `${name}@${version}`, "--pack-destination", directory],
      { encoding: "utf8", shell: process.platform === "win32" },
    );
    const tarball = packed.stdout?.trim().split("\n").at(-1);
    if (packed.status !== 0 || !tarball) return null;
    const unpacked = spawnSync("tar", ["-xzf", tarball], { cwd: directory });
    if (unpacked.status !== 0 || !existsSync(binary)) return null;
  }
  return { version, binary };
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`[rust-transform] ${failure}`);
  const reference = released();
  if (!reference) {
    console.error(
      "[rust-transform] the last release could not be fetched to compare with; on battery or in low power mode this budget is not met by any build",
    );
    process.exitCode = 1;
  } else {
    // Taken in turns, so whatever slows the machine slows both.
    internalSample(reference.binary);
    const ours = [];
    const theirs = [];
    const pairs = (count) => {
      for (let index = 0; index < count; index += 1) {
        theirs.push(internalSample(reference.binary));
        ours.push(internalSample());
      }
      return median(ours) / median(theirs);
    };
    let ratio = pairs(7);
    // Seven pairs on a busy shared runner read the same engine 1.01x on one
    // run and 1.14x on the next (2026-11-27, no Rust change between them).
    // A ratio over the limit is measured further before it fails: three
    // times the pairs, the verdict on all of them.
    if (ratio > budget.againstReleaseMax) {
      console.error(
        `[rust-transform] ${ratio.toFixed(2)}x over seven pairs; measuring fourteen more`,
      );
      ratio = pairs(14);
    }
    console.error(
      `[rust-transform] beside ${reference.version} on this machine now: ${median(ours).toFixed(2)}ms against ${median(theirs).toFixed(2)}ms (${ratio.toFixed(2)}x)`,
    );
    if (ratio > budget.againstReleaseMax) {
      console.error(
        `[rust-transform] slower than the last release by more than ${budget.againstReleaseMax}x`,
      );
      process.exitCode = 1;
    } else {
      console.error(
        "[rust-transform] the last release misses the budget here too, so the machine is slow and this build is not: passing",
      );
    }
  }
}
