#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, relative, resolve } from "node:path";

const toolRoot = resolve(".");
const cli = resolve(toolRoot, "dist/cli.js");
const rustBinary = resolve(
  toolRoot,
  `target/debug/supercov${process.platform === "win32" ? ".exe" : ""}`,
);
const engineFlag = process.argv.indexOf("--engine");
const engine = engineFlag === -1 ? "reference" : process.argv[engineFlag + 1];
if (!new Set(["reference", "rust"]).has(engine))
  throw new Error("--engine must be reference or rust");

function launch(commandArguments, options = {}) {
  const executable = engine === "rust" ? rustBinary : process.execPath;
  const arguments_ = [
    ...(engine === "rust" ? [] : [cli]),
    "--",
    ...commandArguments,
  ];
  return {
    executable,
    arguments_,
    options: {
      ...options,
      env: {
        ...process.env,
        ...(engine === "rust"
          ? { SUPERCOV_RUNTIME_ROOT: resolve(toolRoot, "dist") }
          : {}),
        ...options.env,
      },
    },
  };
}

function snapshot(root) {
  const files = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = resolve(directory, entry.name);
      const local = relative(root, path);
      const separator = process.platform === "win32" ? "\\" : "/";
      // Supercov owns the dotted store and the non-dotted workspace container.
      if (
        [".supercov", "supercov"].some(
          (owned) => local === owned || local.startsWith(`${owned}${separator}`),
        )
      )
        continue;
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile())
        files.push([
          local,
          createHash("sha256").update(readFileSync(path)).digest("hex"),
        ]);
    }
  }
  visit(root);
  return files.sort(([left], [right]) => left.localeCompare(right));
}

const root = mkdtempSync(resolve(tmpdir(), "supercov-fs-crash-"));
try {
  mkdirSync(resolve(root, "src"));
  writeFileSync(
    resolve(root, "package.json"),
    '{"name":"crash-recovery","private":true,"type":"module"}\n',
  );
  writeFileSync(
    resolve(root, "src/decision.js"),
    "export const decision = (a, b) => a && b;\n",
  );
  writeFileSync(
    resolve(root, "test.mjs"),
    'import { decision } from "./src/decision.js"; decision(true, true);\n',
  );
  const padding = resolve(root, "padding");
  mkdirSync(padding);
  for (let index = 0; index < 2_000; index += 1)
    writeFileSync(resolve(padding, `${index}.txt`), `${index}\n`);

  const before = snapshot(root);
  const workspace = resolve(
    root,
    "supercov/workspace",
    basename(root),
  );
  const cacheParent = resolve(workspace, "..");
  const stagingPrefix = `.${basename(root)}.staging-`;
  mkdirSync(workspace, { recursive: true });
  const marker = resolve(workspace, "previous-generation.txt");
  writeFileSync(marker, "last complete generation\n");

  const crashedLaunch = launch([process.execPath, "test.mjs"], {
    cwd: root,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const child = spawn(
    crashedLaunch.executable,
    crashedLaunch.arguments_,
    crashedLaunch.options,
  );
  let output = "";
  child.stdout.on("data", (chunk) => (output += chunk.toString()));
  child.stderr.on("data", (chunk) => (output += chunk.toString()));
  const exit = new Promise((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolveExit({ code, signal }));
  });
  await new Promise((resolveKill, reject) => {
    let observed = false;
    const timeout = setTimeout(() => {
      child.kill("SIGKILL");
      reject(new Error(`cache staging was not observable:\n${output}`));
    }, 30_000);
    const poll = setInterval(() => {
      const staging = existsSync(cacheParent)
        ? readdirSync(cacheParent).some((entry) => entry.startsWith(stagingPrefix))
        : false;
      const lock = existsSync(resolve(root, ".supercov/locks/active.json"));
      if (!staging || !lock) return;
      observed = true;
      clearInterval(poll);
      clearTimeout(timeout);
      child.kill("SIGKILL");
      resolveKill();
    }, 2);
    child.once("exit", (code, signal) => {
      clearInterval(poll);
      clearTimeout(timeout);
      if (!observed)
        reject(
          new Error(
            `crash target exited before staging was observed (${code ?? signal}):\n${output}`,
          ),
        );
    });
  });
  const killed = await exit;
  if (killed.code === 0)
    throw new Error(`crash target exited successfully instead of being killed:\n${output}`);
  if (!existsSync(marker))
    throw new Error("forced termination removed the last complete generation");
  if (!existsSync(resolve(root, ".supercov/locks/active.json")))
    throw new Error("forced termination unexpectedly ran cooperative cleanup");

  const recoveredLaunch = launch([process.execPath, "test.mjs"], {
    cwd: root,
    encoding: "utf8",
    stdio: "pipe",
  });
  const recovered = spawnSync(
    recoveredLaunch.executable,
    recoveredLaunch.arguments_,
    recoveredLaunch.options,
  );
  if (recovered.status !== 0)
    throw new Error(
      `post-kill recovery failed:\n${recovered.stderr}\n${recovered.stdout}`,
    );
  if (existsSync(marker))
    throw new Error("recovery did not publish a fresh cache generation");
  if (JSON.stringify(snapshot(root)) !== JSON.stringify(before))
    throw new Error("crash recovery changed a file outside the Supercov store");

  const runs = readdirSync(resolve(root, ".supercov/runs"));
  if (runs.length !== 1)
    throw new Error(`expected one recovered published run, found ${runs}`);
  const run = resolve(root, ".supercov/runs", runs[0]);
  for (const artifact of ["evidence.raw.gz", "run.json"])
    if (!existsSync(resolve(run, artifact)))
      throw new Error(`recovered run is missing ${artifact}`);
  if (existsSync(resolve(run, "report.json.gz")))
    throw new Error("recovered run persisted a derived report");
  if (existsSync(resolve(root, ".supercov/work", runs[0])))
    throw new Error("recovered run retained terminal work state");
  if (existsSync(resolve(root, ".supercov/evidence", runs[0])))
    throw new Error("recovered run retained loose evidence");
  if (statSync(resolve(run, "evidence.raw.gz")).size === 0)
    throw new Error("recovered run published an empty evidence archive");
  console.log(
    `[filesystem] ${engine} crash recovery passed on ${process.platform}`,
  );
} finally {
  rmSync(root, { recursive: true, force: true });
}
