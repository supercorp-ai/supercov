#!/usr/bin/env node
// The quick check: the tests a change most likely affects, picked from the
// files changed since the merge base, each with the reason it was picked.
// `.github/workflows/quick-check.yml` runs it on Linux, macOS and Windows side
// by side, after `cargo fmt` and clippy. It is a pre-check, not a replacement
// for CI: a test that reaches a changed file without naming it is not picked.
//
//   node scripts/quick-check.mjs [--base <ref>] [--run]
//
// Without `--run` it prints the selection and the commands. The rules:
//
//   - a changed crate source: the crate's own unit tests;
//   - an integration test that names a changed module (`module.rs`, as fault
//     pins do, or `module::`), every test of a file whose `use` names it, and
//     every test of a file named after it (`tests/go_run.rs` for `go_run.rs`);
//   - an integration test whose own lines changed, and those that call a
//     changed helper of a test file;
//   - an integration test, Node test or script that names a changed runtime,
//     launcher or script file;
//   - a changed Node test, a changed end-to-end gate script, the self-test of
//     a changed script (`x-test.mjs` for `x.mjs`), and the checks of what the
//     packages ship (docs, contracts, the launcher, the package files).
//
// The integration tests picked are held to a budget of test-seconds, as
// `.github/test-seconds.tsv` times them (a test not listed counts ten): the
// tests whose own lines changed first, then the cheapest. Those past it are
// named, and left to CI.

import { execFileSync, spawnSync } from "node:child_process";
import { appendFileSync, existsSync, readFileSync, readdirSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const BUDGET = Number(process.env.QUICK_CHECK_BUDGET ?? 900);
const UNKNOWN_SECONDS = 10;

const args = process.argv.slice(2);
const option = (name) => {
  const at = args.indexOf(name);
  return at === -1 ? undefined : args[at + 1];
};
const run = args.includes("--run");
const base = option("--base") ?? "origin/main";

const git = (...rest) => execFileSync("git", rest, { cwd: repository, encoding: "utf8" });

const mergeBase = git("merge-base", base, "HEAD").trim();
const changed = git("diff", "--name-only", "--no-renames", "--diff-filter=d", mergeBase, "HEAD")
  .split("\n")
  .filter(Boolean);

// The lines of each changed file that are new or changed, from a zero-context diff.
function changedLines(file) {
  const lines = new Set();
  const diff = git("diff", "-U0", "--no-renames", mergeBase, "HEAD", "--", file);
  for (const match of diff.matchAll(/^@@ -\S+ \+(\d+)(?:,(\d+))? @@/gm)) {
    const start = Number(match[1]);
    const count = match[2] === undefined ? 1 : Number(match[2]);
    // A pure removal touches the line it was removed before.
    for (let line = start; line < start + Math.max(count, 1); line++) lines.add(line);
  }
  return lines;
}

// Each top-level function of a Rust test file: its name, whether it is a
// test, and its lines (attributes and doc comment included).
function functionsOf(source) {
  const lines = source.split("\n");
  const functions = [];
  for (let index = 0; index < lines.length; index++) {
    const header = /^(?:pub(?:\([^)]*\))? )?(?:async )?fn ([A-Za-z0-9_]+)/.exec(lines[index]);
    if (!header) continue;
    let first = index;
    let test = false;
    while (first > 0 && /^(#\[|\/\/\/)/.test(lines[first - 1])) {
      first--;
      if (lines[first].startsWith("#[test")) test = true;
    }
    let last = index;
    while (last < lines.length && lines[last] !== "}") last++;
    functions.push({ name: header[1], test, first: first + 1, last: last + 1, body: lines.slice(first, last + 1).join("\n") });
    index = last;
  }
  return functions;
}

const crates = readdirSync(join(repository, "crates"));
const packageOf = (crate) => /^name = "([^"]+)"/m.exec(readFileSync(join(repository, "crates", crate, "Cargo.toml"), "utf8"))[1];

// The integration test files: every top-level `tests/*.rs` but the gates,
// which are run as scripts here.
const testFiles = [];
for (const crate of crates) {
  const directory = join(repository, "crates", crate, "tests");
  if (!existsSync(directory)) continue;
  for (const name of readdirSync(directory).sort()) {
    if (!name.endsWith(".rs") || name === "e2e_gates.rs") continue;
    const path = `crates/${crate}/tests/${name}`;
    const source = readFileSync(join(repository, path), "utf8");
    const functions = functionsOf(source);
    const outside = source
      .split("\n")
      .filter((_, index) => !functions.some((f) => index + 1 >= f.first && index + 1 <= f.last))
      .join("\n");
    testFiles.push({ crate, package: packageOf(crate), binary: name.slice(0, -3), path, functions, outside });
  }
}

const seconds = new Map();
for (const line of readFileSync(join(repository, ".github/test-seconds.tsv"), "utf8").split("\n")) {
  if (!line || line.startsWith("#")) continue;
  const [name, wall] = line.split("\t");
  seconds.set(name, Number(wall));
}

const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

// Picked integration tests by `package binary name`, each with its reasons
// and whether its own lines changed.
const picked = new Map();
function pick(file, test, reason, own = false) {
  const key = `${file.package}\t${file.binary}\t${test.name}`;
  const entry = picked.get(key) ?? { file, test, reasons: [], own: false };
  if (!entry.reasons.includes(reason)) entry.reasons.push(reason);
  entry.own ||= own;
  picked.set(key, entry);
}

const unitPackages = new Map();
const nodeTests = new Map();
const scripts = new Map();
const npmScripts = new Map();
const unmatched = [];
const note = (map, key, reason) => map.set(key, [...(map.get(key) ?? []), reason]);

const nodeTestFiles = ["tests/runtime", "tests/release", "tests/action"].flatMap((directory) =>
  readdirSync(join(repository, directory))
    .filter((name) => name.endsWith(".test.mjs"))
    .map((name) => `${directory}/${name}`),
);
const gates = new Set(
  [...readFileSync(join(repository, "crates/supercov-cli/tests/e2e_gates.rs"), "utf8").matchAll(/=> "([^"]+\.mjs)"/g)].map(
    (match) => `scripts/${match[1]}`,
  ),
);
const packageScripts = JSON.parse(readFileSync(join(repository, "package.json"), "utf8")).scripts;
const testedScripts = new Set(
  Object.entries(packageScripts)
    .filter(([name]) => name.startsWith("test:"))
    .flatMap(([, command]) => [...command.matchAll(/node (scripts\/[^ ]+\.mjs)/g)].map((match) => match[1])),
);

// The tests that name a file: by `stem.rs` or `stem::` for a Rust module,
// by its file name for anything else.
function pickNaming(pattern, what) {
  for (const file of testFiles) {
    if (file.binary === what.stem) {
      for (const test of file.functions.filter((f) => f.test)) pick(file, test, `in ${file.path}, named after ${what.path}`);
      continue;
    }
    if (what.rust && pattern.test(file.outside)) {
      for (const test of file.functions.filter((f) => f.test)) pick(file, test, `in ${file.path}, which imports ${what.stem}`);
      continue;
    }
    for (const test of file.functions) if (test.test && pattern.test(test.body)) pick(file, test, `names ${what.path}`);
  }
}

for (const path of changed) {
  const crate = /^crates\/([^/]+)\/src\/(.+)\.rs$/.exec(path);
  if (crate) {
    note(unitPackages, crate[1], `${path} changed`);
    let stem = basename(crate[2]);
    if (stem === "mod") stem = basename(dirname(crate[2]));
    const named = stem === "main" || stem === "lib" ? `\\b${stem}\\.rs\\b` : `\\b${escape(stem)}(\\.rs\\b|::)`;
    pickNaming(new RegExp(named), { path, stem, rust: !["main", "lib"].includes(stem) });
    continue;
  }
  const file = testFiles.find((candidate) => candidate.path === path);
  if (file) {
    const lines = changedLines(path);
    const touched = file.functions.filter((f) => [...lines].some((line) => line >= f.first && line <= f.last));
    for (const test of touched.filter((f) => f.test)) pick(file, test, `its lines in ${path} changed`, true);
    for (const helper of touched.filter((f) => !f.test)) {
      const calls = new RegExp(`\\b${helper.name}\\b`);
      for (const test of file.functions) if (test.test && calls.test(test.body)) pick(file, test, `calls ${helper.name}, changed in ${path}`);
    }
    if (touched.length === 0) unmatched.push(`${path} (no test or helper's lines changed)`);
    continue;
  }
  const common = /^crates\/([^/]+)\/tests\/common\//.exec(path);
  if (common) {
    const source = readFileSync(join(repository, path), "utf8");
    const lines = changedLines(path);
    const helpers = source
      .split("\n")
      .map((text, index) => ({ text, line: index + 1 }))
      .filter(({ text }) => /^\s*(pub(\([^)]*\))? )?fn /.test(text));
    const names = new Set();
    for (const [index, helper] of helpers.entries()) {
      const end = helpers[index + 1]?.line ?? Infinity;
      if ([...lines].some((line) => line >= helper.line && line < end)) names.add(/fn ([A-Za-z0-9_]+)/.exec(helper.text)[1]);
    }
    for (const name of names) {
      const calls = new RegExp(`\\b${name}\\b`);
      for (const file of testFiles.filter((candidate) => candidate.crate === common[1]))
        for (const test of file.functions) if (test.test && calls.test(test.body)) pick(file, test, `calls ${name}, changed in ${path}`);
    }
    if (names.size === 0) unmatched.push(`${path} (no helper's lines changed)`);
    continue;
  }
  let matched = false;
  if (nodeTestFiles.includes(path)) {
    note(nodeTests, path, "changed");
    matched = true;
  }
  if (/^(runtime|bin|scripts|cmd)\//.test(path)) {
    const name = basename(path);
    const pattern = new RegExp(`(^|[^A-Za-z0-9_.-])${escape(name)}\\b`);
    const before = picked.size;
    pickNaming(pattern, { path, stem: undefined, rust: false });
    matched ||= picked.size > before;
    for (const test of nodeTestFiles)
      if (pattern.test(readFileSync(join(repository, test), "utf8"))) {
        note(nodeTests, test, `names ${path}`);
        matched = true;
      }
  }
  // A runtime shim: the tests named for what it hooks (`playwright` for
  // `playwrightStrong.mjs`), as few name the shim's file.
  const shim = /^runtime\/[^/]+\/([a-z]+)[A-Za-z0-9]*\.m?[jt]s$/.exec(path);
  if (shim) {
    const word = new RegExp(`(^|_)${shim[1]}(_|$)`);
    for (const file of testFiles)
      for (const test of file.functions) if (test.test && word.test(test.name)) pick(file, test, `named for ${shim[1]}, which ${path} hooks`);
    matched = true;
  }
  // A fixture: the tests and gate scripts that name its directory.
  const fixture = /^tests\/fixtures\/([^/]+)\//.exec(path);
  if (fixture) {
    const names = new RegExp(`fixtures[/\\\\"', ]+${escape(fixture[1])}\\b`);
    const before = picked.size;
    pickNaming(names, { path: `tests/fixtures/${fixture[1]}`, stem: undefined, rust: false });
    matched ||= picked.size > before;
    for (const script of [...gates, ...testedScripts])
      if (existsSync(join(repository, script)) && names.test(readFileSync(join(repository, script), "utf8"))) {
        note(scripts, script, `names tests/fixtures/${fixture[1]}`);
        matched = true;
      }
  }
  // The compatibility corpus: what reads it.
  if (path.startsWith("tests/compat/")) {
    for (const file of testFiles.filter((candidate) => candidate.binary === "compat"))
      for (const test of file.functions) if (test.test) pick(file, test, "reads tests/compat, which changed");
    matched = true;
  }
  if (gates.has(path) || testedScripts.has(path)) {
    note(scripts, path, "a changed test gate");
    matched = true;
  }
  const selfTest = path.replace(/\.mjs$/, "-test.mjs");
  if (path.startsWith("scripts/") && selfTest !== path && existsSync(join(repository, selfTest))) {
    note(scripts, selfTest, `the self-test of ${path}`);
    matched = true;
  }
  if (/^scripts\/.*-test\.mjs$/.test(path)) {
    note(scripts, path, "changed");
    matched = true;
  }
  if (/^(docs\/[^/]+\.md|contracts\/|runtime\/|crates\/[^/]+\/(assets|test-assets|runtime-assets)\/)/.test(path)) {
    note(npmScripts, "test:rust-assets", `${path} is shipped in the crates`);
    matched = true;
  }
  if (path.startsWith("cmd/") || path === "go.mod") {
    note(npmScripts, "test:launcher", `${path} is the Go launcher`);
    matched = true;
  }
  if (path === "package.json" || path.startsWith("npm/") || path.startsWith("bin/")) {
    note(scripts, "scripts/package-preflight.mjs", `${path} is in the npm package`);
    matched = true;
  }
  if (path === "Cargo.toml" || path === "Cargo.lock" || path === "rust-toolchain.toml" || /^crates\/[^/]+\/(Cargo\.toml|build\.rs)$/.test(path)) {
    for (const crate of crates) note(unitPackages, crate, `${path} changed`);
    matched = true;
  }
  if (!matched) unmatched.push(path);
}

// The budget: the tests whose own lines changed always, then the cheapest.
const costOf = (entry) => seconds.get(entry.test.name) ?? UNKNOWN_SECONDS;
const ordered = [...picked.values()].sort((a, b) => Number(b.own) - Number(a.own) || costOf(a) - costOf(b) || a.test.name.localeCompare(b.test.name));
const chosen = [];
const leftOut = [];
let spent = 0;
for (const entry of ordered) {
  if (!entry.own && spent + costOf(entry) > BUDGET) {
    leftOut.push(entry);
    continue;
  }
  spent += costOf(entry);
  chosen.push(entry);
}

// One nextest run a package: its unit tests if its sources changed, and the
// picked tests of each of its test binaries.
const commands = [];
for (const crate of crates) {
  const name = packageOf(crate);
  const unit = unitPackages.has(crate);
  const tests = chosen.filter((entry) => entry.file.crate === crate);
  if (!unit && tests.length === 0) continue;
  const targets = [];
  const filters = [];
  if (unit) {
    targets.push(existsSync(join(repository, "crates", crate, "src/lib.rs")) ? "--lib" : "--bins");
    filters.push("kind(lib)", "kind(bin)");
  }
  for (const binary of [...new Set(tests.map((entry) => entry.file.binary))].sort()) {
    targets.push("--test", binary);
    const names = tests.filter((entry) => entry.file.binary === binary).map((entry) => `test(=${entry.test.name})`);
    filters.push(`(binary(=${binary}) & (${names.join(" | ")}))`);
  }
  commands.push(["cargo", "nextest", "run", "-p", name, ...targets, "--no-fail-fast", "--no-tests=pass", "-E", filters.join(" | ")]);
}
if (scripts.size > 0 && [...scripts.keys()].some((script) => gates.has(script) || testedScripts.has(script)))
  commands.push(["cargo", "build", "-p", "supercov"]);
for (const script of [...scripts.keys()].sort()) commands.push(["node", script]);
if (nodeTests.size > 0) commands.push(["node", "--test", ...[...nodeTests.keys()].sort()]);
for (const script of [...npmScripts.keys()].sort()) commands.push(["npm", "run", script]);

// What was picked, and why.
const out = [];
out.push(`Changed since ${base} (${mergeBase.slice(0, 12)}): ${changed.length} files`);
for (const [crate, reasons] of unitPackages) out.push(`  unit tests of ${packageOf(crate)}: ${reasons.join("; ")}`);
for (const entry of chosen)
  out.push(`  ${entry.file.binary}::${entry.test.name} (${costOf(entry)} s): ${entry.reasons.join("; ")}`);
for (const [script, reasons] of scripts) out.push(`  ${script}: ${reasons.join("; ")}`);
for (const [test, reasons] of nodeTests) out.push(`  ${test}: ${reasons.join("; ")}`);
for (const [script, reasons] of npmScripts) out.push(`  npm run ${script}: ${reasons.join("; ")}`);
out.push(`Integration tests: ${chosen.length} picked, about ${Math.round(spent)} test-seconds of a ${BUDGET} budget`);
if (leftOut.length > 0) {
  out.push(`Left to CI, past the budget (${leftOut.length}):`);
  for (const entry of leftOut) out.push(`  ${entry.file.binary}::${entry.test.name} (${costOf(entry)} s): ${entry.reasons.join("; ")}`);
}
if (unmatched.length > 0) {
  out.push("Changed files no rule picks a test for (fmt and clippy still check them):");
  for (const path of unmatched) out.push(`  ${path}`);
}
out.push("Commands:");
for (const command of commands) out.push(`  ${command.map((part) => (/[\s|&()]/.test(part) ? `'${part}'` : part)).join(" ")}`);
const report = out.join("\n");
console.log(report);
if (process.env.GITHUB_STEP_SUMMARY && run && process.env.QUICK_CHECK_SUMMARY)
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, `### Quick check selection\n\n\`\`\`\n${report}\n\`\`\`\n`);

if (run) {
  let failed = 0;
  for (const [program, ...rest] of commands) {
    console.log(`\n$ ${program} ${rest.join(" ")}`);
    const started = Date.now();
    const result = spawnSync(program, rest, { cwd: repository, stdio: "inherit", shell: process.platform === "win32" && program === "npm" });
    console.log(`(${Math.round((Date.now() - started) / 1000)} s, exit ${result.status ?? result.signal})`);
    if (result.status !== 0) failed++;
  }
  process.exit(failed === 0 ? 0 : 1);
}
