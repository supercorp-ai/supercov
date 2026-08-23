#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { relative, resolve } from "node:path";
import { gunzipSync } from "node:zlib";

function filesUnder(root, directory = root) {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    if (entry.name === ".supercov" || entry.name === "node_modules") return [];
    const path = resolve(directory, entry.name);
    return entry.isDirectory() ? filesUnder(root, path) : [path];
  });
}

function snapshot(root) {
  return Object.fromEntries(
    filesUnder(root)
      .sort()
      .map((path) => [
        relative(root, path),
        createHash("sha256")
          .update(String(statSync(path).mode))
          .update(readFileSync(path))
          .digest("hex"),
      ]),
  );
}

const temporary = mkdtempSync(resolve(tmpdir(), "supercov-packed-npx-"));
try {
  const packed = spawnSync(
    "npm",
    ["pack", "--json", "--pack-destination", temporary],
    { cwd: resolve("."), encoding: "utf8", stdio: "pipe" },
  );
  if (packed.status !== 0)
    throw new Error(`npm pack failed:\n${packed.stderr}\n${packed.stdout}`);
  const tarballName = JSON.parse(packed.stdout)[0]?.filename;
  if (!tarballName) throw new Error(`npm pack did not report a tarball: ${packed.stdout}`);
  const tarball = resolve(temporary, tarballName);
  const project = resolve(temporary, "project");
  cpSync(resolve("tests/fixtures/no-build-node"), project, {
    recursive: true,
    filter: (path) => !path.split(/[\\/]/).includes(".supercov"),
  });
  const before = snapshot(project);
  const executed = spawnSync(
    "npx",
    ["--yes", `--package=${tarball}`, "supercov", "--", "npm", "test"],
    { cwd: project, encoding: "utf8", stdio: "pipe" },
  );
  if (executed.status !== 0)
    throw new Error(
      `packed npx execution failed:\n${executed.stderr}\n${executed.stdout}`,
    );
  const after = snapshot(project);
  if (JSON.stringify(after) !== JSON.stringify(before))
    throw new Error("packed npx execution modified the clean project");

  const runsRoot = resolve(project, ".supercov/runs");
  const runIds = existsSync(runsRoot) ? readdirSync(runsRoot).sort() : [];
  if (runIds.length !== 1)
    throw new Error(`expected one packed npx run, received ${runIds}`);
  if (
    readdirSync(resolve(runsRoot, runIds[0])).some((file) =>
      file.endsWith(".html"),
    )
  )
    throw new Error("packed npx run generated an implicit HTML report");
  if (existsSync(resolve("/tmp/supercov-server-evidence", runIds[0])))
    throw new Error("packed npx run leaked evidence into the global temp directory");
  const metadata = JSON.parse(
    readFileSync(resolve(runsRoot, runIds[0], "run.json"), "utf8"),
  );
  for (const phase of [
    "initializationMs",
    "workspacePreparationMs",
    "adapterSetupMs",
    "instrumentedBuildMs",
    "testCommandMs",
    "reportPreparationMs",
  ]) {
    if (!Number.isFinite(metadata.timings?.[phase]) || metadata.timings[phase] < 0)
      throw new Error(`packed npx run is missing ${phase}`);
  }
  const report = JSON.parse(
    gunzipSync(
      readFileSync(resolve(runsRoot, runIds[0], "report.json.gz")),
    ),
  );
  for (const metric of ["lines", "statements", "functions", "branches"]) {
    if (report.summary[metric].percentage !== 100)
      throw new Error(`${metric} coverage was not complete`);
  }
  if (report.summary.conditionCoveragePct !== 100)
    throw new Error("MC/DC coverage was not complete");
  console.log(
    `[packed-npx] ${tarballName}: clean no-build project, unchanged sources, 100% coverage`,
  );
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
