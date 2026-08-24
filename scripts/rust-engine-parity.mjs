#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { cpSync, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { resolve, relative } from "node:path";
import { readEvidenceArchive } from "../dist/evidenceArchive.js";

const repository = resolve(new URL("..", import.meta.url).pathname);
const fixture = resolve(repository, "tests/fixtures/generic-playwright");
const binary = process.env.SUPERCOV_RUST_BINARY ?? resolve(repository, "target/debug/supercov");
const temporary = mkdtempSync(resolve(repository, ".rust-engine-parity-"));

function execute(cwd, args, rust) {
  const result = spawnSync(process.execPath, [resolve(repository, "bin/supercov.js"), ...args], {
    cwd,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
    env: {
      ...process.env,
      ...(rust
        ? { SUPERCOV_ENGINE: "rust", SUPERCOV_RUST_BINARY: binary }
        : { SUPERCOV_ENGINE: "typescript" }),
    },
  });
  if (result.error) throw result.error;
  if (result.status !== 0)
    throw new Error(
      `${rust ? "Rust" : "TypeScript"} engine command failed (${result.status}):\n${result.stderr}\n${result.stdout}`,
    );
  return result.stdout;
}

function oneRun(project) {
  const runs = readdirSync(resolve(project, ".supercov/runs")).sort();
  if (runs.length !== 1) throw new Error(`Expected one run in ${project}, received ${runs}`);
  return runs[0];
}

function query(project, run, resource, rust) {
  return JSON.parse(
    execute(project, ["runs", run, "coverage", ...resource, "--json"], rust),
  );
}

function normalized(value, run) {
  if (Array.isArray(value)) return value.map((entry) => normalized(entry, run));
  if (value && typeof value === "object")
    return Object.fromEntries(
      Object.entries(value)
        .filter(([key]) => key !== "generatedAt")
        .map(([key, entry]) => [key, normalized(entry, run)]),
    );
  return value === run ? "<run-id>" : value;
}

try {
  const projects = {
    typescript: resolve(temporary, "typescript"),
    rust: resolve(temporary, "rust"),
  };
  for (const project of Object.values(projects))
    cpSync(fixture, project, {
      recursive: true,
      filter: (path) =>
        !relative(fixture, path)
          .split(/[\\/]/)
          .some((segment) => segment === ".supercov" || segment === "supercov"),
    });

  execute(projects.typescript, ["--", "npm", "test"], false);
  execute(projects.rust, ["--", "npm", "test"], true);
  const runs = {
    typescript: oneRun(projects.typescript),
    rust: oneRun(projects.rust),
  };

  const archives = Object.fromEntries(
    Object.entries(projects).map(([engine, project]) => [
      engine,
      readEvidenceArchive(resolve(project, ".supercov/runs", runs[engine], "evidence.raw.gz")),
    ]),
  );
  const manifests = Object.fromEntries(
    Object.entries(archives).map(([engine, archive]) => [
      engine,
      archive.files.find((entry) => entry.path === "manifest.json")?.contents,
    ]),
  );
  if (!manifests.typescript || manifests.typescript !== manifests.rust)
    throw new Error("Rust and TypeScript evidence archives contain different manifests");

  for (const resource of [[], ["files"], ["gaps"]]) {
    const typescript = normalized(
      query(projects.typescript, runs.typescript, resource, false),
      runs.typescript,
    );
    const rust = normalized(query(projects.rust, runs.rust, resource, true), runs.rust);
    if (JSON.stringify(typescript) !== JSON.stringify(rust))
      throw new Error(
        `Rust/TypeScript query mismatch for coverage ${resource.join(" ") || "summary"}\nTypeScript=${JSON.stringify(typescript)}\nRust=${JSON.stringify(rust)}`,
      );
  }
  console.log(
    "[rust-engine-parity] exact manifest and summary/files/gaps query parity passed for Vitest + Playwright",
  );
} finally {
  rmSync(temporary, { recursive: true, force: true, maxRetries: 20, retryDelay: 25 });
}
