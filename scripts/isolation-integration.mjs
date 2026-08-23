#!/usr/bin/env node

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve("tests/fixtures/generic-playwright");
const build = spawnSync("npm", ["run", "build"], {
  cwd: root,
  encoding: "utf8",
  stdio: "pipe",
});
if (build.status !== 0)
  throw new Error(`fixture build failed:\n${build.stderr}\n${build.stdout}`);

function snapshot(directory) {
  const entries = [];
  function visit(path, relativePath = "") {
    for (const entry of readdirSync(path, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const absolute = resolve(path, entry.name);
      const name = relativePath ? `${relativePath}/${entry.name}` : entry.name;
      if (name === ".supercov" || name.startsWith(".supercov/") || name === "node_modules" || name.startsWith("node_modules/"))
        continue;
      if (entry.isDirectory()) visit(absolute, name);
      else if (entry.isFile()) {
        const stat = statSync(absolute);
        entries.push({
          name,
          size: stat.size,
          mtimeMs: stat.mtimeMs,
          sha256: createHash("sha256").update(readFileSync(absolute)).digest("hex"),
        });
      }
    }
  }
  visit(directory);
  return entries;
}

const projectBefore = snapshot(root);
const workRoot = resolve(root, ".supercov/work");
const runsBefore = new Set(existsSync(workRoot) ? readdirSync(workRoot) : []);
const child = spawn(
  process.execPath,
  [resolve("bin/supercov.js"), "--", process.execPath, "-e", "setInterval(() => {}, 1000)"],
  { cwd: root, stdio: ["ignore", "pipe", "pipe"] },
);
let output = "";
let signalled = false;
const observe = (chunk) => {
  output += chunk.toString();
  if (!signalled && output.includes("[supercov] running in isolated workspace")) {
    signalled = true;
    child.kill("SIGTERM");
  }
};
child.stdout.on("data", observe);
child.stderr.on("data", observe);

const exit = await new Promise((resolveExit, reject) => {
  const timeout = setTimeout(() => {
    child.kill("SIGKILL");
    reject(new Error(`signal cleanup timed out:\n${output}`));
  }, 20_000);
  child.once("error", reject);
  child.once("exit", (code, signal) => {
    clearTimeout(timeout);
    resolveExit({ code, signal });
  });
});
if (!signalled) throw new Error(`run never entered its test phase:\n${output}`);
if (exit.code !== 143 && exit.signal !== "SIGTERM")
  throw new Error(`unexpected interrupted exit ${JSON.stringify(exit)}:\n${output}`);

const newRuns = (existsSync(workRoot) ? readdirSync(workRoot) : []).filter(
  (id) => !runsBefore.has(id),
);
if (newRuns.length !== 1)
  throw new Error(`expected one interrupted run, found ${newRuns.join(", ")}`);
const runId = newRuns[0];
const state = JSON.parse(readFileSync(resolve(workRoot, runId, "state.json"), "utf8"));
if (state.status !== "interrupted" || state.signal !== "SIGTERM")
  throw new Error(`unexpected run state: ${JSON.stringify(state)}`);
if (existsSync(state.workspace))
  throw new Error(`isolated workspace survived SIGTERM: ${state.workspace}`);
if (existsSync(resolve(root, ".supercov/locks/active.json")))
  throw new Error("project lock survived SIGTERM");
const projectAfter = snapshot(root);
if (JSON.stringify(projectAfter) !== JSON.stringify(projectBefore))
  throw new Error("a project file outside .supercov changed during the interrupted coverage run");

const storedRunsRoot = resolve(root, ".supercov/runs");
const storedRunsBefore = new Set(
  existsSync(storedRunsRoot) ? readdirSync(storedRunsRoot) : [],
);
const successful = spawnSync(
  process.execPath,
  [resolve("bin/supercov.js"), "--", "npm", "run", "test:unit"],
  { cwd: root, encoding: "utf8", stdio: "pipe" },
);
if (successful.status !== 0)
  throw new Error(
    `successful isolated run failed:\n${successful.stderr}\n${successful.stdout}`,
  );
const publishedRuns = (existsSync(storedRunsRoot)
  ? readdirSync(storedRunsRoot)
  : []
).filter((id) => !storedRunsBefore.has(id));
if (publishedRuns.length !== 1)
  throw new Error(`expected one atomically published run, found ${publishedRuns.join(", ")}`);
const publishedFiles = new Set(
  readdirSync(resolve(storedRunsRoot, publishedRuns[0])),
);
for (const required of [
  "report.json.gz",
  "report.html",
  "report-passed.html",
  "report-failed.html",
  "run.json",
]) {
  if (!publishedFiles.has(required))
    throw new Error(`atomically published run is missing ${required}`);
}
if (
  existsSync(
    resolve(root, ".supercov/work", publishedRuns[0], "report-publication"),
  )
)
  throw new Error("report staging directory survived atomic publication");
const projectAfterSuccess = snapshot(root);
if (JSON.stringify(projectAfterSuccess) !== JSON.stringify(projectBefore))
  throw new Error("a project file outside .supercov changed during a successful coverage run");

console.log(
  `[isolation] SIGTERM recovered run ${runId}; successful and interrupted runs left every project file outside .supercov unchanged`,
);
