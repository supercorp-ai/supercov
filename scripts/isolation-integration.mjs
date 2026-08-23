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
import { basename, resolve } from "node:path";

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

async function verifyUncatchableCacheRecovery() {
  const crashRoot = mkdtempSync(resolve(tmpdir(), "supercov-crash-recovery-"));
  try {
    mkdirSync(resolve(crashRoot, "src"));
    writeFileSync(
      resolve(crashRoot, "package.json"),
      '{"name":"crash-recovery","private":true,"type":"module"}\n',
    );
    writeFileSync(
      resolve(crashRoot, "src/decision.js"),
      "export const decision = (a, b) => a && b;\n",
    );
    writeFileSync(
      resolve(crashRoot, "test.mjs"),
      'import { decision } from "./src/decision.js"; decision(true, true);\n',
    );
    const padding = resolve(crashRoot, "padding");
    mkdirSync(padding);
    for (let index = 0; index < 1_000; index += 1)
      writeFileSync(resolve(padding, `${index}.txt`), `${index}\n`);

    const projectBefore = snapshot(crashRoot);
    const expectedCache = resolve(
      crashRoot,
      ".supercov/cache/instrumented-workspace",
      basename(crashRoot),
    );
    const cacheParent = resolve(expectedCache, "..");
    const stagingPrefix = `.${basename(crashRoot)}.staging-`;
    mkdirSync(expectedCache, { recursive: true });
    const previousGenerationMarker = resolve(
      expectedCache,
      "previous-generation.txt",
    );
    writeFileSync(previousGenerationMarker, "last complete generation\n");

    const killed = spawn(
      process.execPath,
      [resolve("dist/cli.js"), "--", process.execPath, "test.mjs"],
      { cwd: crashRoot, stdio: ["ignore", "pipe", "pipe"] },
    );
    let killedOutput = "";
    killed.stdout.on("data", (chunk) => (killedOutput += chunk.toString()));
    killed.stderr.on("data", (chunk) => (killedOutput += chunk.toString()));
    const killedExit = new Promise((resolveExit) =>
      killed.once("exit", (code, signal) => resolveExit({ code, signal })),
    );
    await new Promise((resolveKill, reject) => {
      const timeout = setTimeout(() => {
        killed.kill("SIGKILL");
        reject(new Error(`cache preparation was not observable:\n${killedOutput}`));
      }, 20_000);
      const poll = setInterval(() => {
        const staging = existsSync(cacheParent)
          ? readdirSync(cacheParent).some((entry) =>
              entry.startsWith(stagingPrefix),
            )
          : false;
        const ownsLock = existsSync(
          resolve(crashRoot, ".supercov/locks/active.json"),
        );
        if (!staging || !ownsLock) return;
        clearInterval(poll);
        clearTimeout(timeout);
        killed.kill("SIGKILL");
        resolveKill();
      }, 2);
      killed.once("exit", (code, signal) => {
        if (signal === "SIGKILL") return;
        clearInterval(poll);
        clearTimeout(timeout);
        reject(
          new Error(
            `preparation process exited before SIGKILL (${code ?? signal}):\n${killedOutput}`,
          ),
        );
      });
      killed.once("error", reject);
    });
    const killedResult = await killedExit;
    if (killedResult.signal !== "SIGKILL")
      throw new Error(`expected SIGKILL, received ${JSON.stringify(killedResult)}`);
    if (!existsSync(previousGenerationMarker))
      throw new Error("SIGKILL replaced or removed the last complete generation");
    if (!existsSync(resolve(crashRoot, ".supercov/locks/active.json")))
      throw new Error("SIGKILL unexpectedly ran cooperative lock cleanup");

    const recovered = spawnSync(
      process.execPath,
      [resolve("dist/cli.js"), "--", process.execPath, "test.mjs"],
      { cwd: crashRoot, encoding: "utf8", stdio: "pipe" },
    );
    if (recovered.status !== 0)
      throw new Error(
        `post-SIGKILL recovery failed:\n${recovered.stderr}\n${recovered.stdout}`,
      );
    if (existsSync(previousGenerationMarker))
      throw new Error("the recovered run did not publish a fresh cache generation");
    if (JSON.stringify(snapshot(crashRoot)) !== JSON.stringify(projectBefore))
      throw new Error("SIGKILL recovery changed a file outside .supercov");
  } finally {
    rmSync(crashRoot, { recursive: true, force: true });
  }
}

await verifyUncatchableCacheRecovery();

const projectBefore = snapshot(root);
const workRoot = resolve(root, ".supercov/work");
const runsBefore = new Set(existsSync(workRoot) ? readdirSync(workRoot) : []);
const expectedCache = resolve(
  root,
  ".supercov/cache/instrumented-workspace/generic-playwright",
);

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
if (existsSync(resolve("/tmp/supercov-server-evidence", runId)))
  throw new Error("interrupted CLI run leaked evidence into the global temp directory");
if (state.workspace !== expectedCache || !existsSync(state.workspace))
  throw new Error(
    `interrupted run did not remain confined to the stable isolated cache: ${state.workspace}`,
  );
const interruptedMarker = resolve(state.workspace, "interrupted-marker.txt");
writeFileSync(interruptedMarker, "must be removed by the next refresh\n");
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
if (existsSync(interruptedMarker))
  throw new Error("the next run did not refresh interrupted isolated output");
const publishedRuns = (existsSync(storedRunsRoot)
  ? readdirSync(storedRunsRoot)
  : []
).filter((id) => !storedRunsBefore.has(id));
if (publishedRuns.length !== 1)
  throw new Error(`expected one atomically published run, found ${publishedRuns.join(", ")}`);
if (existsSync(resolve("/tmp/supercov-server-evidence", publishedRuns[0])))
  throw new Error("successful CLI run leaked evidence into the global temp directory");
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

const cleaned = spawnSync(
  process.execPath,
  [resolve("bin/supercov.js"), "clean", "--keep", "20"],
  { cwd: root, encoding: "utf8", stdio: "pipe" },
);
if (cleaned.status !== 0)
  throw new Error(`isolated cache cleanup failed:\n${cleaned.stderr}\n${cleaned.stdout}`);
if (existsSync(expectedCache))
  throw new Error("supercov clean left the isolated build cache behind");

console.log(
  `[isolation] SIGKILL preserved the prior cache generation, the next run recovered its transaction, SIGTERM remained cooperative, clean removed all cache data, and no project file outside .supercov changed`,
);
