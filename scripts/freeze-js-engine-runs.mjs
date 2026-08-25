#!/usr/bin/env node

import { cpSync, existsSync, mkdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dirname, "..");
const destination = resolve(root, "contracts/js-engine-runs-v1");
const runs = {
  "generic-playwright": ["2026-08-25T12-44-38-755Z", "2026-08-25T12-44-43-742Z"],
  "generic-node": [
    "2026-08-25T00-23-44-430Z",
    "2026-08-25T00-23-44-871Z",
    "rust-direct-node-1",
    "rust-direct-node-2",
  ],
  "generic-esbuild": ["2026-08-25T12-33-56-557Z", "2026-08-25T12-33-57-205Z"],
  "generic-webpack": ["2026-08-25T00-23-38-498Z", "2026-08-25T00-23-39-434Z"],
  "generic-swc": ["2026-08-25T00-23-39-938Z", "2026-08-25T00-23-40-607Z"],
};

for (const [fixture, ids] of Object.entries(runs)) {
  for (const id of ids) {
    const source = resolve(root, "tests/fixtures", fixture, ".supercov/runs", id);
    const target = resolve(destination, fixture, id);
    if (!existsSync(resolve(source, "run.json")) || !existsSync(resolve(source, "evidence.raw.gz")))
      throw new Error(`missing source run ${fixture}/${id}`);
    if (existsSync(target)) {
      for (const name of ["run.json", "evidence.raw.gz"])
        if (!readFileSync(resolve(source, name)).equals(readFileSync(resolve(target, name))))
          throw new Error(`frozen run changed ${fixture}/${id}/${name}`);
      continue;
    }
    mkdirSync(target, { recursive: true });
    for (const name of ["run.json", "evidence.raw.gz"])
      cpSync(resolve(source, name), resolve(target, name), { errorOnExist: true });
  }
}

console.log(`[js-engine-runs] froze ${Object.values(runs).flat().length} immutable runs`);
