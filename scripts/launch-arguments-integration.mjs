#!/usr/bin/env node
// What reaches Supercov's native binary when a person starts it through a
// package manager, a runtime or a shell: each way of starting it, with the
// arguments people write, must hand the binary exactly those arguments.
// Bun's script runner drops the `--` before a script's own arguments
// (oven-sh/bun#13984), PowerShell drops it on the way into npm's `.ps1`
// shim, and cmd re-reads `%` and `&`: the binary then took the test command
// for one of its own.
//
// The binary is replaced by a probe that records its arguments, through the
// launcher's own SUPERCOV_RUST_BINARY; everything in front of it is real:
// the packed npm package installed by each package manager, their shims, the
// gem's wrapper, the wheel's script.
//
//   node scripts/launch-arguments-integration.mjs [--report <file.json>] [--only <substring>]
//
// Exits 1 when an expected case differs; a launcher that is not installed is
// skipped and named.
import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const repository = resolve(import.meta.dirname, "..");
const windows = process.platform === "win32";
const options = process.argv.slice(2);
const reportPath = valueOf("--report");
const only = valueOf("--only");

function valueOf(flag) {
  const index = options.indexOf(flag);
  return index === -1 ? undefined : options[index + 1];
}

// A space in the root: launchers that hand a path to a shell unquoted fail
// here as they would in a home directory with one.
const root = mkdtempSync(join(tmpdir(), "supercov launch "));
const results = [];

function run(command, args, settings = {}) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    stdio: "pipe",
    timeout: 300_000,
    ...settings,
  });
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error,
  };
}

function must(command, args, settings) {
  const result = run(command, args, settings);
  if (result.status !== 0)
    throw new Error(
      `${command} ${args.join(" ")} failed (${result.status ?? result.error?.message}):\n${result.stderr}\n${result.stdout}`,
    );
  return result;
}

function available(command, args = ["--version"]) {
  const result = run(command, args, { shell: windows });
  return result.status === 0;
}

// The probe: a native executable, as the real binary is, that writes the
// arguments it was given as one JSON line.
function buildProbe() {
  const source = join(root, "probe.rs");
  writeFileSync(
    source,
    `use std::io::Write;
fn main() {
    let mut line = String::from("[");
    for (index, argument) in std::env::args_os().skip(1).enumerate() {
        if index > 0 { line.push(','); }
        line.push('"');
        for character in argument.to_string_lossy().chars() {
            match character {
                '"' => line.push_str("\\\\\\""),
                '\\\\' => line.push_str("\\\\\\\\"),
                c if (c as u32) < 0x20 => line.push_str(&format!("\\\\u{:04x}", c as u32)),
                c => line.push(c),
            }
        }
        line.push('"');
    }
    line.push(']');
    let path = std::env::var_os("SUPERCOV_ARGUMENTS_PROBE").expect("SUPERCOV_ARGUMENTS_PROBE");
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}
`,
  );
  const probe = join(root, windows ? "probe.exe" : "probe");
  must("rustc", ["-O", "-o", probe, source]);
  return probe;
}

const probe = buildProbe();
const record = join(root, "arguments.jsonl");

function packed() {
  const result = must(
    "npm",
    ["pack", "--ignore-scripts", "--json", "--pack-destination", root],
    { cwd: repository, shell: windows },
  );
  return join(root, JSON.parse(result.stdout)[0].filename);
}

const tarball = packed();

// Arguments every launcher must carry, in two spellings: one any shell reads
// the same way (package scripts run in sh on Unix and cmd on Windows), and
// arguments that only survive a shell when quoted for it.
const plain = [
  { name: "separator", args: ["--", "tool", "run"] },
  { name: "exact attribution", args: ["--exact-attribution", "--", "tool", "run"] },
  { name: "separator inside the command", args: ["--", "tool", "--", "--flag"] },
  { name: "subcommand", args: ["runs", "latest", "gaps", "--limit", "5"] },
  { name: "help", args: [] },
  { name: "a dashed test command", args: ["--", "--weird", "x"] },
];
const quoted = [
  { name: "space", args: ["--", "tool", "a b"] },
  { name: "empty", args: ["--", "tool", ""] },
  { name: "ampersand", args: ["--", "tool", "x&y"] },
  { name: "percent", args: ["--", "tool", "100%"] },
  { name: "caret", args: ["--", "tool", "a^b"] },
  { name: "non-ASCII", args: ["--", "tool", "ünï"] },
  { name: "inner quote", args: ["--", "tool", 'say "hi"'] },
];

// How the shell a person types into writes one argument.
function quote(shell, argument) {
  if (shell === "sh") {
    if (/^[\w@%+=:,./-]+$/u.test(argument)) return argument;
    return `'${argument.replaceAll("'", `'\\''`)}'`;
  }
  if (shell === "pwsh") {
    if (/^[\w@+=:,./-]+$/u.test(argument) && argument !== "--") return argument;
    if (argument === "--") return "--";
    return `'${argument.replaceAll("'", "''")}'`;
  }
  // cmd: quotes keep spaces and `&`; `%` and `^` are only literal inside
  // quotes in a script, and an inner quote is doubled by convention for
  // programs built on the Microsoft C runtime and Node.
  if (/^[\w@+=:,./-]+$/u.test(argument)) return argument;
  return `"${argument.replaceAll('"', '\\"')}"`;
}

function line(shell, words) {
  return words.map((word) => quote(shell, word)).join(" ");
}

function shellRun(shell, commandLine, cwd, env) {
  if (shell === "sh") return run("/bin/sh", ["-c", commandLine], { cwd, env });
  if (shell === "pwsh")
    return run("pwsh", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", commandLine], {
      cwd,
      env,
    });
  return run(process.env.ComSpec ?? "cmd.exe", ["/d", "/s", "/c", `"${commandLine}"`], {
    cwd,
    env,
    windowsVerbatimArguments: true,
  });
}

function observe(launcher, shell, cwd, commandLine, expected, extraEnv = {}) {
  const name = `${launcher} [${shell}] ${commandLine}`;
  if (only && !name.includes(only)) return;
  rmSync(record, { force: true });
  const env = {
    ...process.env,
    SUPERCOV_RUST_BINARY: probe,
    SUPERCOV_ARGUMENTS_PROBE: record,
    ...extraEnv,
  };
  const result = shellRun(shell, commandLine, cwd, env);
  const lines = existsSync(record) ? readFileSync(record, "utf8").trim().split("\n") : [];
  const received = lines.length > 0 ? JSON.parse(lines.at(-1)) : null;
  const ok = received !== null && JSON.stringify(received) === JSON.stringify(expected);
  results.push({
    launcher,
    shell,
    commandLine,
    expected,
    received,
    ok,
    status: result.status,
    output: ok ? undefined : `${result.stderr}\n${result.stdout}`.trim().slice(-800),
  });
}

function skip(launcher, reason) {
  results.push({ launcher, skipped: reason });
}

function project(name, manifest, files = {}) {
  const directory = join(root, name);
  mkdirSync(directory, { recursive: true });
  writeFileSync(
    join(directory, "package.json"),
    `${JSON.stringify({ name: `launch-${name}`, private: true, version: "1.0.0", ...manifest }, null, 2)}\n`,
  );
  for (const [file, content] of Object.entries(files)) writeFileSync(join(directory, file), content);
  return directory;
}

const shells = windows ? ["cmd", "pwsh"] : ["sh"];
if (!windows && available("pwsh", ["-NoProfile", "-Command", "exit 0"])) shells.push("pwsh");

// Package scripts: what a person writes in package.json, run by each runner.
// A script's text is run by sh, or by cmd on Windows, so it uses only the
// arguments both read alike.
const scriptCases = [
  ...plain.filter((each) => each.args.length > 0),
  { name: "space", args: ["--", "tool", "a b"] },
  { name: "ampersand", args: ["--", "tool", "x&y"] },
];
const scripts = Object.fromEntries(
  scriptCases.map((each, index) => [
    `case${index}`,
    `supercov ${each.args.map((argument) => (/^[\w@%+=:,./-]+$/u.test(argument) ? argument : `"${argument}"`)).join(" ")}`,
  ]),
);

const managers = [
  { name: "npm", manifest: {}, install: ["npm", ["install", "--ignore-scripts", "--omit=optional", "--no-audit", "--no-fund", tarball]] },
  { name: "pnpm", manifest: { packageManager: "pnpm@10.12.1" }, install: ["pnpm", ["add", "--ignore-scripts", "--no-optional", tarball]] },
  { name: "yarn1", manifest: { packageManager: "yarn@1.22.22" }, install: ["yarn", ["add", "--ignore-scripts", "--ignore-optional", `file:${tarball}`]] },
  {
    name: "yarn4",
    manifest: { packageManager: "yarn@4.9.2" },
    files: { ".yarnrc.yml": "nodeLinker: node-modules\nenableScripts: false\n", "yarn.lock": "" },
    install: ["yarn", ["add", `supercov@file:${tarball}`]],
  },
  { name: "bun", manifest: {}, install: ["bun", ["add", tarball]] },
  { name: "bun-run-bun", manifest: {}, files: { "bunfig.toml": "[run]\nbun = true\n" }, install: ["bun", ["add", tarball]] },
];

for (const manager of managers) {
  const tool = manager.install[0];
  if (!available(tool)) {
    skip(manager.name, `${tool} is not installed`);
    continue;
  }
  const directory = project(manager.name, { ...manager.manifest, scripts }, manager.files);
  const installed = run(manager.install[0], manager.install[1], {
    cwd: directory,
    shell: windows,
    env: { ...process.env, COREPACK_ENABLE_DOWNLOAD_PROMPT: "0" },
  });
  if (installed.status !== 0) {
    skip(manager.name, `install failed: ${(installed.stderr || installed.stdout).trim().slice(-400)}`);
    continue;
  }
  const runners = {
    npm: [["npm", "run"], ["npm", "run", "--silent"]],
    pnpm: [["pnpm", "run"], ["pnpm"]],
    yarn1: [["yarn", "run"], ["yarn"]],
    yarn4: [["yarn", "run"], ["yarn"]],
    bun: [["bun", "run"], ["bun", "--bun", "run"]],
    "bun-run-bun": [["bun", "run"]],
  }[manager.name];
  for (const shell of shells) {
    // The script as written.
    scriptCases.forEach((each, index) => {
      for (const runner of runners)
        observe(manager.name, shell, directory, line(shell, [...runner, `case${index}`]), each.args);
    });
    // Arguments added after the script's name reach the end of its command.
    for (const runner of runners) {
      const forward =
        runner[0] === "npm" || (runner[0] === "yarn" && manager.name === "yarn1") ? ["--", "--extra"] : ["--extra"];
      observe(manager.name, shell, directory, line(shell, [...runner, "case0", ...forward]), [...scriptCases[0].args, "--extra"]);
    }
    // The binary started directly, as `npx supercov` or `pnpm exec supercov`.
    const direct = {
      npm: [["npx", "supercov"], ["npm", "exec", "--", "supercov"], [join("node_modules", ".bin", "supercov")]],
      pnpm: [["pnpm", "exec", "supercov"], ["pnpm", "supercov"]],
      yarn1: [["yarn", "supercov"]],
      yarn4: [["yarn", "supercov"], ["yarn", "exec", "supercov"]],
      bun: [["bunx", "supercov"], ["bunx", "--bun", "supercov"], ["bun", "x", "supercov"], ["bun", "run", "supercov"], ["bun", "--bun", "run", "supercov"]],
      "bun-run-bun": [["bun", "run", "supercov"], ["bunx", "supercov"]],
    }[manager.name];
    for (const prefix of direct) {
      if (prefix[0].startsWith("node_modules") && shell === "cmd") prefix[0] = prefix[0].replaceAll("/", "\\");
      for (const each of [...plain, ...quoted]) {
        if (shell === "cmd" && ["percent", "caret", "inner quote"].includes(each.name)) continue;
        observe(manager.name, shell, directory, line(shell, [...prefix, ...each.args]), each.args);
      }
    }
  }
}

// deno runs package.json scripts as tasks, in its own shell.
if (available("deno")) {
  const directory = join(root, "npm");
  if (existsSync(join(directory, "node_modules")))
    scriptCases.forEach((each, index) => {
      observe("deno", windows ? "cmd" : "sh", directory, line(windows ? "cmd" : "sh", ["deno", "task", `case${index}`]), each.args);
    });
} else skip("deno", "deno is not installed");

// The gem's wrapper, under a directory with a space and a shell
// metacharacter in its name.
if (available("ruby")) {
  const gem = join(root, "gem dir (x)");
  mkdirSync(join(gem, "exe"), { recursive: true });
  mkdirSync(join(gem, "libexec"), { recursive: true });
  copyFileSync(join(repository, "packaging/rubygems/exe/supercov"), join(gem, "exe/supercov"));
  copyFileSync(probe, join(gem, "libexec", windows ? "supercov.exe" : "supercov"));
  if (!windows) chmodSync(join(gem, "libexec/supercov"), 0o755);
  for (const shell of shells)
    for (const each of [...plain, ...quoted]) {
      if (shell === "cmd" && ["percent", "caret", "inner quote"].includes(each.name)) continue;
      observe("gem wrapper", shell, gem, line(shell, ["ruby", join(gem, "exe", "supercov"), ...each.args]), each.args);
    }
} else skip("gem wrapper", "ruby is not installed");

// The wheel's script, through uv and pipx.
const wheels = join(root, "wheels");
const triple = must("rustc", ["-vV"]).stdout.match(/^host: (.+)$/m)?.[1];
const builtWheel = run(process.execPath, [join(repository, "scripts/build-wheel.mjs"), "--target", triple, "--binary", probe, "--out", wheels], {
  cwd: repository,
});
const wheel = builtWheel.status === 0 ? readdirSync(wheels).find((file) => file.endsWith(".whl")) : undefined;
if (!wheel) skip("wheel", `the wheel was not built: ${builtWheel.stderr.trim().slice(-400)}`);
else
  for (const [launcher, prefix] of [
    ["uvx", ["uvx", "--from", join(wheels, wheel), "supercov"]],
    ["uv run", ["uv", "run", "--no-project", "--with", join(wheels, wheel), "supercov"]],
    ["pipx", ["pipx", "run", "--spec", join(wheels, wheel), "supercov"]],
  ]) {
    if (!available(prefix[0])) {
      skip(launcher, `${prefix[0]} is not installed`);
      continue;
    }
    for (const shell of shells)
      for (const each of [...plain, ...quoted]) {
        if (shell === "cmd" && ["percent", "caret", "inner quote"].includes(each.name)) continue;
        observe(launcher, shell, root, line(shell, [...prefix, ...each.args]), each.args);
      }
  }

const failed = results.filter((each) => each.ok === false);
const passed = results.filter((each) => each.ok === true);
for (const each of results) {
  if (each.skipped) console.log(`SKIP ${each.launcher}: ${each.skipped}`);
  else if (!each.ok)
    console.log(
      `FAIL ${each.launcher} [${each.shell}] ${each.commandLine}\n  expected ${JSON.stringify(each.expected)}\n  received ${JSON.stringify(each.received)} (exit ${each.status})${each.output ? `\n  ${each.output.split("\n").slice(-3).join("\n  ")}` : ""}`,
    );
}
console.log(`[launch-arguments] ${passed.length} passed, ${failed.length} failed, ${results.filter((each) => each.skipped).length} skipped on ${process.platform}`);
if (reportPath) writeFileSync(reportPath, `${JSON.stringify(results, null, 2)}\n`);
rmSync(root, { recursive: true, force: true });
if (failed.length > 0) process.exitCode = 1;
